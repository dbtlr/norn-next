//! **The churned tree, read the same way by every suite that churns one.**
//!
//! Two suites in this crate put a live vault through the churn driver's
//! workload families: one asks whether the host converges on what a build from
//! zero holds, and one asks how long converging takes. Both need the same two
//! things, and they are here so that the two suites cannot answer them
//! differently.
//!
//! **The ground.** A workload family is written against facts a script cannot
//! invent — what the volume does with two spellings, how much frontmatter the
//! text layer reads, and how a vault's own declaration is spelled. [`ground`]
//! answers all three for a directory.
//!
//! **The wait.** The cheapest signal that says a host has caught up is the
//! places a walk reads and the content hash each one implies, held against the
//! rows the store derived. That is the [`Census`], and it is not a bar: what a
//! store holds is bodies, links, headings, blocks, tags, indexed terms,
//! findings and the pinned schema, and only `norn_testkit::equivalence` reads
//! those. A suite stops waiting here and judges there.
#![allow(dead_code)] // Two integration targets read this module, and each uses the part its own wait needs.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: reads the suite's own generated tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{CaseSensitivity, ContentHash, PathNormalizer};
use norn_store::{DocumentPath, Store};
use norn_testkit::churn::{self, Folding, Ground, SchemaGround, Script};

use crate::attach;

/// The bytes family 4's third phase replaces the vault's schema declaration
/// with.
///
/// A schema is opaque to derivation — it is read, hashed and pinned, and no
/// document is judged against it — so what makes this a schema *change* is that
/// the bytes differ from the ones the attachment pinned. The pin is what
/// discards every finding derived under the old fingerprint, and the heal that
/// follows it is what derives them again.
pub const REPLACEMENT_SCHEMA: &[u8] = b"version: 1\n# a second declaration\n";

/// Where a vault's own declaration sits, and what replaces it.
const SCHEMA_GROUND: SchemaGround<'static> = SchemaGround {
    at: ".norn/schema.yaml",
    replacement: REPLACEMENT_SCHEMA,
};

/// The facts the workload families are built against, for a tree under `at`.
pub fn ground(at: &Path) -> Ground<'static> {
    Ground {
        folding: declared_folding(at),
        frontmatter_read_bound: norn_text::FRONTMATTER_MAX_BYTES,
        schema: SCHEMA_GROUND,
    }
}

/// **The platform lane, declared.** What the volume under `at` does with case,
/// asked two ways and required to agree.
///
/// A suite churning against one answer while the host served the other would be
/// judging a tree neither of them describes, so the probe that writes a file
/// and looks for it under another spelling is held against the normalizer a
/// host itself resolves paths with.
pub fn declared_folding(at: &Path) -> Folding {
    let probed = churn::folding(at).expect("a case probe over the sandbox");
    let normalizer = PathNormalizer::detect(at).expect("a normalizer over the sandbox");
    let resolved = match normalizer.case_sensitivity() {
        CaseSensitivity::Insensitive => Folding::Folded,
        CaseSensitivity::Sensitive => Folding::Distinct,
    };
    assert_eq!(
        probed, resolved,
        "a probe that wrote a file and looked for it under another spelling says this is {probed}, \
         and the normalizer a host resolves paths with says it is {resolved}"
    );
    eprintln!("this suite is running against {probed}");
    probed
}

/// What the tree holds, read as places rather than as documents.
///
/// A place is a markdown file a walk reads. Whether it derives a row is decided
/// here the same way a heal decides it: a name the document-path grammar
/// refuses derives none, bytes no decoder accepts derive none, and everything
/// else derives one holding the hash of the bytes on disk.
///
/// **A place is keyed by its identity, not by its spelling.** On a volume that
/// folds case, `Note.md` and `note.md` are one place, and a census comparing the
/// two renderings byte for byte would call a document that never moved a place
/// with no row standing beside a row standing nowhere. Whether two derivations
/// agree about the *spelling* a document is rendered at is the equivalence
/// comparator's question, asked of the whole projection rather than of this
/// coarse signal.
///
/// **What keying by identity costs is duplicates.** Two derived rows whose
/// spellings fold together collapse into one entry here, so a store holding
/// both would look to this wait exactly like a store holding the right one. That
/// is a limit of the signal and not a gap in the suite: the equivalence
/// comparator reads every row of both projections, and the case that flips a
/// name's case asks directly how many rows stand at the flipped identity — and
/// asks the directory itself what spelling it renders there.
///
/// **Two readings are compared for equality**, which is how a phase that
/// applied acts and moved nothing is caught: the places, the hashes, the places
/// that derive none and the vault's schema declaration all take part, because
/// each of them is something a changing phase may be the only mover of.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Census {
    /// What the volume does with case, which is what makes two spellings one
    /// place or two.
    pub folding: Folding,
    /// The identity of each place, to the hash the bytes there imply.
    pub rows: BTreeMap<String, String>,
    /// The identity of each place that derives no row.
    pub without_rows: BTreeSet<String>,
    /// Each identity's spelling on disk, which is what a failure names.
    pub spellings: BTreeMap<String, String>,
    /// The vault's own schema declaration, as the tree holds it.
    ///
    /// **A schema replacement changes no path and no hash**, so a reading that
    /// stopped at the two above is a reading a phase replacing a declaration
    /// leaves untouched — and the claim that a non-empty phase moved the tree
    /// would pass for a phase that wrote the standing declaration back over
    /// itself. That is the shape a schema-replacing phase fails as, and it is
    /// the shape it should fail as: a re-pin is what the phase exists to drive,
    /// and bytes equal to the ones already there drive none.
    ///
    /// **The store is not asked about it here.** What a host has pinned moves
    /// only when a reload is asked for, and a reload runs the re-pin before it
    /// returns, so nothing about the pin is ever outstanding at a settle. A
    /// wait on it would be a wait on an act that has already finished.
    pub schema: Option<Vec<u8>>,
}

/// A path as its identity on a volume with this case behavior.
///
/// The fold is ASCII, which is the fold the derived store's own path ordering
/// uses: a suite folding more than the store does would call two places one that
/// the store keeps apart.
pub fn identity(path: &str, folding: Folding) -> String {
    match folding {
        Folding::Folded => path.to_ascii_lowercase(),
        Folding::Distinct => path.to_string(),
    }
}

impl Census {
    /// **The workload's declaration and the tree agree.** Every place the script
    /// said derives no row is a place this reading of the tree also finds
    /// derives none.
    ///
    /// The declaration is the driver's, made where the workload is written, and
    /// this reading is made from the bytes on disk afterwards. They are two
    /// answers to one question, so a workload that meant to leave a quarantined
    /// place and left a readable one is caught here rather than passing a bar
    /// about a state it never reached.
    pub fn assert_the_script_read_the_tree_the_same_way(&self, script: &Script) {
        for declared in script.places_without_rows() {
            let place = identity(declared, self.folding);
            assert!(
                self.without_rows.contains(&place),
                "`{}` says `{declared}` derives no row, and the tree there does derive one",
                script.name()
            );
        }
    }

    /// How the store disagrees with the tree, and nothing where they agree.
    ///
    /// Every disagreement is reported rather than the first, because the two
    /// halves of one defect read as two lines: a row at a spelling the tree no
    /// longer holds and a place with no row are the same rename seen from each
    /// end, and a message naming only one of them sends a reader looking for a
    /// document that moved rather than for the move.
    pub fn disagreement(&self, store: &mut Store) -> Option<String> {
        /// How many disagreements one message carries. A workload that
        /// diverged everywhere says so in the count.
        const REPORTED: usize = 8;

        let derived: BTreeMap<String, (String, String)> = derived_hashes(store)
            .into_iter()
            .map(|(path, hash)| (identity(&path, self.folding), (path, hash)))
            .collect();
        let mut apart = Vec::new();
        for (place, hash) in &self.rows {
            let at = &self.spellings[place];
            match derived.get(place) {
                Some((_, held)) if held == hash => {}
                Some((_, held)) => {
                    apart.push(format!("`{at}` holds {held} and the tree holds {hash}"));
                }
                None => apart.push(format!("`{at}` stands in the tree and holds no row")),
            }
        }
        for place in &self.without_rows {
            if derived.contains_key(place) {
                apart.push(format!(
                    "`{}` derives no document and holds a row",
                    self.spellings[place]
                ));
            }
        }
        for (place, (spelling, _)) in &derived {
            if !self.rows.contains_key(place) {
                apart.push(format!(
                    "`{spelling}` holds a row and stands nowhere in the tree"
                ));
            }
        }
        if apart.is_empty() {
            return None;
        }
        let total = apart.len();
        apart.truncate(REPORTED);
        Some(format!("{total} disagreements: {}", apart.join("; ")))
    }
}

/// Read the tree at `root` as places, keyed by identity on a volume with this
/// case behavior.
pub fn census(root: &Path, folding: Folding) -> Census {
    let mut census = Census {
        folding,
        rows: BTreeMap::new(),
        without_rows: BTreeSet::new(),
        spellings: BTreeMap::new(),
        schema: std::fs::read(root.join(".norn/schema.yaml")).ok(),
    };
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|e| panic!("reading {}: {e}", directory.display()));
        for entry in entries {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            let kind = entry.file_type().expect("an entry's type");
            // A symbolic link is not a place: a walk refuses to follow one, so
            // a `.md` link derives nothing however it resolves.
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                // Norn's own subtree carries the schema declaration and the
                // mechanism scratch root, and no document.
                if path.file_name() != Some(std::ffi::OsStr::new(".norn")) {
                    pending.push(path);
                }
                continue;
            }
            let Some(relative) = markdown_place(root, &path) else {
                continue;
            };
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            let place = identity(&relative, folding);
            let derives =
                DocumentPath::new(&relative).is_ok() && std::str::from_utf8(&bytes).is_ok();
            if derives {
                census
                    .rows
                    .insert(place.clone(), ContentHash::of(&bytes).to_string());
            } else {
                census.without_rows.insert(place.clone());
            }
            census.spellings.insert(place, relative);
        }
    }
    census
}

/// The vault-relative spelling of `path`, where it is a markdown file.
fn markdown_place(root: &Path, path: &Path) -> Option<String> {
    if path.extension() != Some(std::ffi::OsStr::new("md")) {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    Some(relative.to_string_lossy().into_owned())
}

/// Every derived path and the hash the row holds.
///
/// The page loop is the attach fixture's, which every suite in this crate reads
/// a whole vault through: the bound on one page is the point of it, and one
/// place to state it is one place to keep it bounded.
pub fn derived_hashes(store: &mut Store) -> BTreeMap<String, String> {
    let mut held = BTreeMap::new();
    attach::for_each_derived_document(store, |document| {
        held.insert(
            document.path.as_str().to_string(),
            document.content_hash.clone(),
        );
    });
    held
}
