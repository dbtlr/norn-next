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
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::mem;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

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
    ///
    /// The root's own entries are read once for the whole call: the walk over
    /// them and the hardlink question every candidate among them asks are
    /// served by [`DirNames`] from one scan, so a wide root full of alternate
    /// spellings costs one listing rather than one per candidate. The probe
    /// against the root's entry in its parent asks the parent one question per
    /// call, so that one streams through [`listing_holds`] and keeps nothing.
    pub fn detect(root: &Path) -> Result<Self, NormalizerError> {
        if let (Some(parent), Some(name)) = (root.parent(), root.file_name())
            && same_device(root, parent)
            && let Some(sensitivity) =
                probe_case_behavior(parent, name, |alternate| listing_holds(parent, alternate))
        {
            return Ok(Self { sensitivity });
        }

        let mut entries = DirNames::opened(root).map_err(|source| NormalizerError::ReadRoot {
            root: root.to_owned(),
            source,
        })?;

        while let Some(name) = entries.next_name() {
            let name = name.map_err(|source| NormalizerError::ReadRoot {
                root: root.to_owned(),
                source,
            })?;
            if alternate_ascii_case(&name).is_none() {
                continue;
            }
            if let Some(sensitivity) =
                probe_case_behavior(root, &name, |alternate| Some(entries.contains(alternate)))
            {
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

/// One directory's entry names, produced by a single scan that every question
/// the walk over that directory asks is answered from.
///
/// Detection asks two kinds of that directory. The walk takes names one at a
/// time looking for a case-bearing candidate, and every candidate whose
/// alternate spelling exists with the same identity asks whether that spelling
/// is itself an entry. The second question is an absence proof over the whole
/// directory, and a root where many entries ask it — hardlink aliases, which is
/// exactly where the question cannot be skipped — would pay a listing per
/// asking entry. Here the first asking takes the rest of the scan the walk is
/// already partway through, and every later one is answered from the names in
/// hand, by lookup rather than by searching them: a root of N alias entries
/// costs one listing and work linear in N, not a listing per entry nor a search
/// of the listing per entry.
///
/// Names are pulled only as they are asked for, so a root whose first candidate
/// settles the case behavior still never lists the rest of itself. What is held
/// is the names one scan has produced, held for the length of the
/// [`PathNormalizer::detect`] call that asked and dropped when it returns; a
/// root that settles on its first entry holds one name, and a root holds its
/// whole width only where a membership question drains the scan, which is where
/// the absence proof is the whole listing. Each held name is one allocation the
/// ordered names and the lookup index share.
///
/// The probe against the root's entry in its parent does not use this. It asks
/// that parent one question per detect call, so there is no later question to
/// answer from held names and nothing is held: it streams through
/// [`listing_holds`].
struct DirNames {
    scan: Scan,
    /// The names the scan produced, in the order it produced them. An entry the
    /// scan could not read holds its place as the error it produced.
    names: Vec<io::Result<Rc<OsStr>>>,
    /// The same names as a membership question asks for them — by value rather
    /// than in order — so an answer costs a lookup rather than a search.
    index: HashSet<Rc<OsStr>>,
    /// How many of `names` the walk has taken.
    walked: usize,
}

/// Where a directory's one scan stands.
enum Scan {
    /// Open, with names it has not produced yet.
    Reading(fs::ReadDir),
    /// Spent: every name it produced is held.
    Spent,
}

impl DirNames {
    /// Names of a directory opened now, so a caller that owes a refusal for a
    /// root it cannot enumerate has the error before it asks for a name.
    #[allow(clippy::disallowed_methods)] // The vault filesystem seam: read-only case detection.
    fn opened(dir: &Path) -> io::Result<Self> {
        let scan = fs::read_dir(dir)?;
        #[cfg(test)]
        count_scan();
        Ok(Self {
            scan: Scan::Reading(scan),
            names: Vec::new(),
            index: HashSet::new(),
            walked: 0,
        })
    }

    /// The next name the walk has not taken, or the error the scan produced in
    /// its place. `None` once the walk has taken every name.
    fn next_name(&mut self) -> Option<io::Result<Rc<OsStr>>> {
        if self.walked == self.names.len() && !self.pull() {
            return None;
        }
        let slot = self.names.get_mut(self.walked)?;
        self.walked += 1;
        Some(match slot {
            Ok(name) => Ok(Rc::clone(name)),
            // The error is handed over and its place is kept, because a
            // membership question passes over an unreadable entry either way.
            failed => mem::replace(failed, Err(io::ErrorKind::Other.into())),
        })
    }

    /// Whether `name` is one of this directory's entries.
    ///
    /// The scan runs on only as far as the name: an answer of yes costs the
    /// names up to it, and only an answer of no costs the whole listing, which
    /// is what establishing an absence costs. Names already produced are
    /// answered by lookup, so a question about a directory the scan has been
    /// drained for costs no reading and no search.
    fn contains(&mut self, name: &OsStr) -> bool {
        loop {
            #[cfg(test)]
            count_name_test();
            if self.index.contains(name) {
                return true;
            }
            if !self.pull() {
                return false;
            }
        }
    }

    /// Takes one more name off the scan. Reports whether a name was added; a
    /// spent scan adds nothing and stays spent.
    fn pull(&mut self) -> bool {
        let Scan::Reading(scan) = &mut self.scan else {
            return false;
        };
        let Some(entry) = scan.next() else {
            self.scan = Scan::Spent;
            return false;
        };
        #[cfg(test)]
        count_dirent();
        match entry {
            Ok(entry) => {
                let name: Rc<OsStr> = Rc::from(entry.file_name().as_os_str());
                self.index.insert(Rc::clone(&name));
                self.names.push(Ok(name));
            }
            Err(error) => self.names.push(Err(error)),
        }
        true
    }
}

/// Whether `dir` holds an entry spelled exactly `name`, or `None` where `dir`
/// cannot be read.
///
/// The scan is taken as a stream and nothing it produces is kept, which is the
/// trade for a caller that asks one question: a second question about the same
/// directory would open a second scan. [`DirNames`] is the other side of it,
/// for the caller that asks a question per entry.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: read-only case detection.
fn listing_holds(dir: &Path, name: &OsStr) -> Option<bool> {
    let scan = fs::read_dir(dir).ok()?;
    #[cfg(test)]
    count_scan();
    for entry in scan {
        #[cfg(test)]
        count_dirent();
        // An entry the scan could not read is passed over: it names no spelling
        // to match, and it is not this question's to refuse on.
        if entry.is_ok_and(|entry| entry.file_name() == name) {
            return Some(true);
        }
    }
    Some(false)
}

/// The case behavior `name`'s entry in `dir` proves, where it proves one.
///
/// `holds` answers the hardlink question — is the alternate spelling itself an
/// entry of `dir`? — and is the caller's because what answering it costs is the
/// caller's: a walk over `dir`'s entries has a scan of it in hand, and a single
/// probe against `dir` has no second question to keep names for. An answer of
/// `None` is a directory that could not be read, which proves nothing.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: read-only case detection.
fn probe_case_behavior(
    dir: &Path,
    name: &OsStr,
    holds: impl FnOnce(&OsStr) -> Option<bool>,
) -> Option<CaseSensitivity> {
    let alternate = alternate_ascii_case(name)?;
    let actual_metadata = fs::symlink_metadata(dir.join(name)).ok()?;

    match fs::symlink_metadata(dir.join(&alternate)) {
        Ok(other) if !same_identity(&actual_metadata, &other) => Some(CaseSensitivity::Sensitive),
        Ok(_) => {
            // If both spellings are actual entries, they may merely be hardlink
            // aliases. Only lookup of a spelling absent from the directory can
            // positively demonstrate case-insensitive name resolution.
            (!holds(&alternate)?).then_some(CaseSensitivity::Insensitive)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(CaseSensitivity::Sensitive),
        Err(_) => None,
    }
}

/// What detection asked of the directories it read and of the names it held,
/// counted for this module's own suite.
///
/// A detect call hands back a case behavior and says nothing about how many
/// times it read a directory, nor about how much of what it read it goes over
/// again; both are the subject of the entry cache above, and neither has any
/// other observable. The counts are thread-local for the reason
/// [`crate::reads`]'s are: what a case reads is what its own thread did, rather
/// than a shared number the cases running beside it also move.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DetectionReads {
    /// Directory scans opened.
    scans: u64,
    /// Entries pulled off those scans, an entry the scan could not read
    /// included.
    dirents: u64,
    /// Membership lookups against the names one scan holds. A question costs
    /// one for each name it waits for and one for its answer, so every question
    /// a directory of N entries can ask costs on the order of N of them
    /// together; searching the held names per question would cost N each.
    name_tests: u64,
}

#[cfg(test)]
thread_local! {
    static DETECTION_READS: std::cell::Cell<DetectionReads> =
        const { std::cell::Cell::new(DetectionReads { scans: 0, dirents: 0, name_tests: 0 }) };
}

#[cfg(test)]
fn count_scan() {
    count(|reads| reads.scans += 1);
}

#[cfg(test)]
fn count_dirent() {
    count(|reads| reads.dirents += 1);
}

#[cfg(test)]
fn count_name_test() {
    count(|reads| reads.name_tests += 1);
}

#[cfg(test)]
fn count(tally: impl FnOnce(&mut DetectionReads)) {
    DETECTION_READS.with(|counts| {
        let mut reads = counts.get();
        tally(&mut reads);
        counts.set(reads);
    });
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

    use norn_testkit::scratch::Scratch;

    fn normalizer(sensitivity: CaseSensitivity) -> PathNormalizer {
        PathNormalizer::for_sensitivity(sensitivity)
    }

    /// The tree a case that reaches the filesystem works in.
    ///
    /// It is held for the length of the case rather than removed at the end of
    /// one: a case here revokes a directory's permissions and asserts before
    /// putting them back, so a failing assertion would otherwise leave the
    /// tree — and, on a host where the assertion fails, every rerun's tree —
    /// on the machine.
    fn scratch() -> Scratch {
        Scratch::new("norn-path")
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
        let tree = scratch();
        let parent = tree.root();
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
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: observing the host filesystem's case behavior.
    fn detection_uses_the_empty_root_entry_as_case_evidence() {
        let tree = scratch();
        let root = tree.root();
        let parent = root.parent().expect("scratch parent");
        let name = root.file_name().expect("scratch name");
        let expected =
            probe_case_behavior(parent, name, |alternate| listing_holds(parent, alternate))
                .expect("case-bearing root entry");

        let detected = PathNormalizer::detect(root).expect("detectable empty root");
        assert_eq!(detected.case_sensitivity(), expected);
    }

    /// What detection read while `work` ran on this thread.
    fn detection_reads(work: impl FnOnce()) -> DetectionReads {
        DETECTION_READS.set(DetectionReads::default());
        work();
        DETECTION_READS.get()
    }

    /// Gives `spelling` a second name at `alias`, reporting whether this host
    /// can hold both at once.
    ///
    /// A host that cannot is a host that folds case, so the two spellings are
    /// one entry there and a case wanting them apart has no shape to run on it.
    /// That the host folds is asserted here rather than read off the failed
    /// link: a link that failed for any other reason is a broken arrangement,
    /// and a case may not pass by calling that a skip. The skip says so on
    /// standard error, so a run watching its output sees the case stand down.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging hardlink aliases.
    fn linked_alias(spelling: &Path, alias: &Path) -> bool {
        if fs::hard_link(spelling, alias).is_ok() {
            return true;
        }
        let folded = fs::symlink_metadata(spelling)
            .ok()
            .zip(fs::symlink_metadata(alias).ok())
            .is_some_and(|(spelled, aliased)| same_identity(&spelled, &aliased));
        assert!(
            folded,
            "{} could not be linked at {}, on a host that keeps the two spellings apart",
            spelling.display(),
            alias.display()
        );
        eprintln!(
            "skipped: this host folds case, so {} is already {}",
            alias.display(),
            spelling.display()
        );
        false
    }

    /// **One detect call reads the root's entries once, and answers every
    /// question about them by lookup.** Every entry holding a same-identity
    /// alternate spelling asks the hardlink question, and each question is
    /// answered from the scan the walk over those entries is already taking —
    /// so a root where many entries ask costs the same one scan, and the same
    /// one listing, as a root where one does.
    ///
    /// The dirent count is exact: no entry here proves anything, so the walk
    /// takes every name, and one scan of the root is what that costs.
    ///
    /// The name tests are a bar, because which names the scan produces first
    /// decides how long the first question waits. Every order stays under two
    /// per entry — the pulls one question waits through are names no later
    /// question waits for again — where answering a question by searching the
    /// names in hand costs the width of the listing per asking entry, which
    /// this root has enough entries to tell apart from the bar.
    ///
    /// A case-insensitive host cannot hold the distinct spellings the repeated
    /// question needs, and skips.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging hardlink aliases.
    fn detection_reads_a_hardlink_ambiguous_root_once() {
        const ALIASES: u64 = 32;
        const ENTRIES: u64 = 2 * ALIASES;
        let tree = scratch();
        let parent = tree.root();
        let root = parent.join("123");
        fs::create_dir(&root).expect("uncased root");
        for index in 0..ALIASES {
            let spelling = root.join(format!("Probe{index}"));
            fs::write(&spelling, b"").expect("probe entry");
            if !linked_alias(&spelling, &root.join(format!("probe{index}"))) {
                return;
            }
        }

        let mut detected = None;
        let reads = detection_reads(|| detected = Some(PathNormalizer::detect(&root)));

        assert!(
            matches!(detected, Some(Err(NormalizerError::Indeterminate { .. }))),
            "a root proving nothing but hardlink aliases is indeterminate"
        );
        assert_eq!(reads.scans, 1, "{reads:?}");
        assert_eq!(reads.dirents, ENTRIES, "{reads:?}");
        assert!(reads.name_tests <= 2 * ENTRIES, "{reads:?}");
    }

    /// **Detection reads no further than the entry that settles it.** Which
    /// entry that is depends on the host: where an alternate spelling does not
    /// resolve, the first entry the scan produces settles the root and the rest
    /// are never read, and no membership question is ever asked; where it
    /// resolves to the same file, settling it means establishing that the
    /// directory does not hold that spelling, which is the whole listing — one
    /// scan of it, and one name test for each name that scan waits on.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: observing the host filesystem's case behavior.
    fn detection_reads_no_further_than_the_entry_that_settles_it() {
        const ENTRIES: u64 = 6;
        let tree = scratch();
        let parent = tree.root();
        let root = parent.join("123");
        fs::create_dir(&root).expect("uncased root");
        for index in 0..ENTRIES {
            fs::write(root.join(format!("Probe{index}")), b"").expect("probe entry");
        }
        let alternate_resolves = fs::symlink_metadata(root.join("probe0")).is_ok();

        let mut detected = None;
        let reads = detection_reads(|| detected = Some(PathNormalizer::detect(&root)));

        assert_eq!(
            detected
                .expect("detection ran")
                .expect("a case-bearing root")
                .case_sensitivity(),
            if alternate_resolves {
                CaseSensitivity::Insensitive
            } else {
                CaseSensitivity::Sensitive
            }
        );
        assert_eq!(
            reads,
            DetectionReads {
                scans: 1,
                dirents: if alternate_resolves { ENTRIES } else { 1 },
                name_tests: if alternate_resolves { ENTRIES } else { 0 },
            }
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging hardlink aliases.
    fn hardlinked_alternate_spellings_do_not_prove_insensitivity() {
        let tree = scratch();
        let parent = tree.root();
        let root = parent.join("123");
        fs::create_dir(&root).expect("uncased root");
        fs::write(root.join("CaseProbe"), b"").expect("probe entry");
        if !linked_alias(&root.join("CaseProbe"), &root.join("caseProbe")) {
            return;
        }

        assert!(matches!(
            PathNormalizer::detect(&root),
            Err(NormalizerError::Indeterminate { .. })
        ));
    }

    /// A root the process cannot enumerate is not an indeterminate root: the
    /// two refusals name different situations, and only this one carries the
    /// error the filesystem gave.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: naming a root that is not there.
    fn detection_refuses_a_root_it_cannot_enumerate() {
        let tree = scratch();
        let parent = tree.root();
        let root = parent.join("123");
        let refusal = PathNormalizer::detect(&root).expect_err("an absent root");
        assert!(
            matches!(&refusal, NormalizerError::ReadRoot { root: named, source }
                if named == &root && source.kind() == io::ErrorKind::NotFound),
            "{refusal:?}"
        );
    }

    /// **A directory that cannot be listed answers no membership question, and
    /// an unanswered question proves nothing.** The hardlink guard is all that
    /// stands between two spellings resolving to one file and the conclusion
    /// that the root folds case, so a listing this process may not read has to
    /// leave the probe unsettled rather than report the alternate spelling
    /// absent.
    ///
    /// The probe against the root's own entry reads the one directory the vault
    /// does not own — its parent — so it is the probe that meets this. Where
    /// the host folds case the whole shape runs: the alternate spelling of the
    /// root's name resolves to the root, the guard asks the unlistable parent,
    /// and detection falls through to the root's own entries, which prove
    /// nothing. Where it does not fold, the alternate spelling is absent and
    /// the probe settles before reaching the question — so the answer of the
    /// unlistable listing is pinned directly as well, on every host.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: revoking a directory's read permission.
    fn an_unlistable_directory_answers_no_membership_question() {
        use std::os::unix::fs::PermissionsExt;

        let tree = scratch();
        let parent = tree.root();
        let holder = parent.join("holder");
        fs::create_dir(&holder).expect("holder directory");
        let root = holder.join("Case");
        fs::create_dir(&root).expect("empty root");
        fs::set_permissions(&holder, fs::Permissions::from_mode(0o111)).expect("revoke read");
        let listable = fs::read_dir(&holder).is_ok();

        let answer = listing_holds(&holder, OsStr::new("case"));
        let folds = fs::symlink_metadata(holder.join("case"))
            .ok()
            .zip(fs::symlink_metadata(&root).ok())
            .is_some_and(|(alternate, root)| same_identity(&alternate, &root));
        let detected = PathNormalizer::detect(&root).map(|paths| paths.case_sensitivity());

        fs::set_permissions(&holder, fs::Permissions::from_mode(0o755)).expect("restore read");

        if listable {
            eprintln!("skipped: this process lists a directory it holds no read permission on");
            return;
        }
        assert_eq!(answer, None, "an unlistable directory holds no known name");
        if folds {
            assert!(
                matches!(detected, Err(NormalizerError::Indeterminate { .. })),
                "an unanswered hardlink guard settles nothing: {detected:?}"
            );
        } else {
            assert!(
                matches!(detected, Ok(CaseSensitivity::Sensitive)),
                "an absent alternate spelling settles the root before the guard: {detected:?}"
            );
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a root without case evidence.
    fn detection_refuses_when_the_root_has_no_case_evidence() {
        let tree = scratch();
        let parent = tree.root();
        let root = parent.join("123");
        fs::create_dir(&root).expect("uncased root");
        fs::write(root.join("123"), b"").expect("uncased entry");
        assert!(matches!(
            PathNormalizer::detect(&root),
            Err(NormalizerError::Indeterminate { .. })
        ));
    }

    #[test]
    fn mount_probe_guard_compares_device_identity() {
        let tree = scratch();
        let root = tree.root();
        assert!(same_device(root, root));
        assert!(!same_device(&root.join("missing"), root));
    }
}
