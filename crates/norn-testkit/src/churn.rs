//! The churn driver: scripted, seeded edits a live vault tree is put through.
//!
//! A host that maintains a vault is judged by what it converges on, and the
//! only way to ask is to change the tree underneath it and then compare what it
//! holds against a derivation that started from zero over the same final tree.
//! This module is the first half of that: the **scripts** — ordered, named acts
//! against a directory — and the machinery that applies them. The comparison is
//! [`crate::equivalence`], and the suite that attaches a host, settles it and
//! judges the pair lives in the crate whose host it is.
//!
//! # Nothing here knows what a vault is
//!
//! A script writes bytes, renames names and removes places. It never opens a
//! store, never attaches a host and never reads a derived row, so what it says
//! about a tree is true of the tree whatever any host did with it. The one
//! norn-shaped thing a script carries is a **declaration**: [`Script::places_without_rows`]
//! names the places the workload put content at that no document row stands for
//! — bytes no decoder accepts, a name norn cannot spell — because a suite
//! judging convergence has to know which places it must *not* find a row at.
//!
//! # Determinism
//!
//! Every byte a script writes comes from its seed. [`Ink`] is a seeded stream
//! and nothing here reads a clock, an environment variable or the operating
//! system's randomness, so two runs of one script at one seed write the same
//! trees in the same order. A step's content is therefore reproducible from the
//! failure message alone.
//!
//! # A failing case prints its script
//!
//! Every [`Step`] carries a sentence saying what it does to the tree, and a
//! [`Script`] renders as its numbered steps. What a suite reports on failure is
//! the workload rather than a stack of `fs::write` calls, and
//! [`Applied::log`] is the same sentences for the steps that really ran — which
//! is the shorter list whenever a suite applies a script in parts.
//!
//! # The families
//!
//! Five constructors build the workload families a churn suite runs, and each
//! is data rather than a procedure: a suite may apply one whole, apply it a step
//! at a time around its own waits, or interleave two. They are
//! [`ordinary_editing`], [`atomic_replacement`] with [`case_flip`] beside it,
//! [`burst`], [`validity_transitions`] and [`external_tools`].
//!
//! Each of them is a [`Phased`] pair rather than one script, because an edit is
//! only an edit against a row a host already holds — see that type for why a
//! workload that landed whole would say nothing but "files appeared".
//!
//! # The writes here are foreign on purpose
//!
//! `norn-fs` owns every write norn makes to a vault, and nothing in this module
//! goes through it. A churn workload is the *other* writer — the editor, the
//! synchronization client, the person at a shell — and a driver that wrote
//! through norn's own protocol would be exercising norn against itself. So the
//! lint that keeps direct filesystem calls out of the workspace is turned off
//! for this module alone, and turned off nowhere else in the testkit's harness
//! machinery than where the tree really is the subject.
#![allow(clippy::disallowed_methods)]

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What a volume does with two spellings that differ only in case.
///
/// A case-flip rename is two different acts on the two kinds of volume — a move
/// to a new name, or a re-spelling of one name — so a suite that means to
/// exercise it declares which volume it is on rather than skipping where the
/// answer is inconvenient. [`folding`] is how it asks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Folding {
    /// Two spellings differing only in case are two places.
    Distinct,
    /// Two spellings differing only in case are one place.
    Folded,
}

impl fmt::Display for Folding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Folding::Distinct => formatter.write_str("a case-sensitive volume"),
            Folding::Folded => formatter.write_str("a case-insensitive volume"),
        }
    }
}

/// The name the probe writes, which is removed before this function returns.
const PROBE: &str = "norn-churn-case-probe";

/// What the volume `at` sits on does with case.
///
/// The probe is a file written and removed inside `at`, so the answer is the
/// volume's own rather than the platform's: a case-sensitive volume mounted on
/// a system whose boot volume folds is what a target-family test would get
/// wrong.
pub fn folding(at: &Path) -> io::Result<Folding> {
    let lower = at.join(PROBE);
    let upper = at.join(PROBE.to_uppercase());
    fs::write(&lower, b"probe")?;
    let folded = upper.exists();
    fs::remove_file(&lower)?;
    Ok(if folded {
        Folding::Folded
    } else {
        Folding::Distinct
    })
}

/// One thing done to a tree.
///
/// Each variant is a single filesystem act, because a step is what a suite may
/// apply on its own — around a wait, between two of a host's polls, or in the
/// middle of another script.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Act {
    /// Write `bytes` at `at`, creating the directories above it.
    Write { at: String, bytes: Vec<u8> },
    /// Write `bytes` to a temporary name beside `at` and rename it over `at`.
    ///
    /// This is the save an editor makes and the landing a synchronization
    /// client makes: the content appears at its name in one act, and the name
    /// never holds a partial document.
    AtomicReplace { at: String, bytes: Vec<u8> },
    /// Move `from` to `to`, creating the directories above `to`.
    Rename { from: String, to: String },
    /// Remove the file at `at`.
    Remove { at: String },
    /// Create the directory `at`, and every directory above it.
    CreateDirectory { at: String },
    /// Remove the directory at `at` and everything under it.
    RemoveDirectory { at: String },
    /// Remove the file at `at` and create a directory in its place.
    ReplaceWithDirectory { at: String },
}

/// The document places `act` changes, read from the tree `act` is about to run
/// against.
///
/// **A directory is not a place a document lives.** A bound stated over the
/// changed set is stated over places a host may have to open, judge and write,
/// and a directory is none of those: what a directory act really changes is the
/// documents standing under it, so those are what this reports and the
/// directory's own name is not. Counting the name as well would let a workload
/// inflate its own changed set by naming directories, which is a bound growing
/// to fit its reading.
///
/// **A rename names two places per document it moves** — the name that emptied
/// and the name that filled — because a host owes work at both ends of a move.
/// A rename of a directory is therefore the same account as renaming each
/// document under it one at a time, which is what it is.
///
/// The tree is read here rather than after the act, because the documents a
/// directory act changes are the ones standing under it while it still stands.
fn document_places(root: &Path, act: &Act) -> Vec<String> {
    match act {
        Act::Write { at, .. }
        | Act::AtomicReplace { at, .. }
        | Act::Remove { at }
        | Act::ReplaceWithDirectory { at } => documents([at.clone()]),
        Act::CreateDirectory { .. } => Vec::new(),
        Act::RemoveDirectory { at } => documents_under(root, at),
        Act::Rename { from, to } if root.join(from).is_dir() => documents_under(root, from)
            .into_iter()
            .flat_map(|moved| {
                let tail = moved[from.len()..].to_string();
                [moved.clone(), format!("{to}{tail}")]
            })
            .collect(),
        Act::Rename { from, to } => documents([from.clone(), to.clone()]),
    }
}

/// Those of `named` that are markdown places, which is where a document lives.
fn documents(named: impl IntoIterator<Item = String>) -> Vec<String> {
    named.into_iter().filter(|at| at.ends_with(".md")).collect()
}

/// Every vault-relative markdown place standing under the directory `at`.
///
/// A symbolic link is not one: a vault walk refuses to follow one, so a `.md`
/// link under a renamed directory is no document a host owes work at.
fn documents_under(root: &Path, at: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![at.to_string()];
    while let Some(relative) = pending.pop() {
        let Ok(entries) = fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let under = format!("{relative}/{name}");
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(under),
                Ok(kind) if kind.is_file() && under.ends_with(".md") => found.push(under),
                _ => {}
            }
        }
    }
    found.sort();
    found
}

/// One act, and the sentence that says what it does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Step {
    says: String,
    act: Act,
}

impl Step {
    pub fn new(says: impl Into<String>, act: Act) -> Step {
        Step {
            says: says.into(),
            act,
        }
    }

    /// What this step does to the tree, in words.
    pub fn says(&self) -> &str {
        &self.says
    }

    pub fn act(&self) -> &Act {
        &self.act
    }
}

impl fmt::Display for Step {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.says)
    }
}

/// A named, ordered workload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Script {
    name: String,
    steps: Vec<Step>,
    places_without_rows: BTreeSet<String>,
}

impl Script {
    pub fn new(name: impl Into<String>, steps: Vec<Step>) -> Script {
        Script {
            name: name.into(),
            steps,
            places_without_rows: BTreeSet::new(),
        }
    }

    /// Declare that no document row stands at `at` once this script has run.
    ///
    /// **This is a cross-check and not an instruction.** A suite reading the
    /// tree afterwards decides for itself which places derive a row — a name
    /// the document-path grammar refuses derives none, and neither do bytes no
    /// decoder accepts — so a convergence wait does not need this declaration
    /// to know where to expect one. What the declaration adds is a second
    /// answer to the same question, made where the workload is written: a
    /// workload that meant to leave a quarantined place and left a readable one
    /// is caught against its own claim rather than passing a bar about a state
    /// it never reached.
    pub fn without_rows_at(mut self, at: impl Into<String>) -> Script {
        self.places_without_rows.insert(at.into());
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The places this script says hold no document row when it has run.
    pub fn places_without_rows(&self) -> &BTreeSet<String> {
        &self.places_without_rows
    }

    /// Apply every step to the tree under `root`, in order.
    pub fn apply(&self, root: &Path) -> Result<Applied, ChurnError> {
        let mut applied = Applied::default();
        for step in &self.steps {
            apply_step(root, step, &mut applied)?;
        }
        Ok(applied)
    }

    /// Apply the steps at `positions`, in order, recording them into `applied`.
    ///
    /// This is how a suite drives a script around its own waits: the run is one
    /// account of what has really happened to the tree, however many calls it
    /// took to get there.
    pub fn apply_range(
        &self,
        root: &Path,
        positions: std::ops::Range<usize>,
        applied: &mut Applied,
    ) -> Result<(), ChurnError> {
        for position in positions {
            let step = self.steps.get(position).unwrap_or_else(|| {
                panic!(
                    "`{}` holds {} steps and a suite asked for step {position}",
                    self.name,
                    self.steps.len()
                )
            });
            apply_step(root, step, applied)?;
        }
        Ok(())
    }
}

impl fmt::Display for Script {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "workload `{}`:", self.name)?;
        for (position, step) in self.steps.iter().enumerate() {
            writeln!(formatter, "  {position}. {step}")?;
        }
        Ok(())
    }
}

/// A workload in the two phases a suite settles between.
///
/// **A modification is only a modification against a row a host already
/// holds.** Everything a script writes between two of a host's polls arrives as
/// one changed set, so a workload whose creations and its edits land together
/// says nothing but "these files appeared": no row is re-derived, none is
/// pruned, and no tombstone is written however many edits and deletions the
/// script spells. Each family here is therefore two scripts — the [`opening`]
/// puts content in the tree, and the [`changing`] acts on the rows a host that
/// settled over the opening is holding by then.
///
/// The phase whose cost a suite brackets is the changing one, because that is
/// the phase whose changed set is the subject.
///
/// [`opening`]: Phased::opening
/// [`changing`]: Phased::changing
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Phased {
    opening: Script,
    changing: Script,
}

impl Phased {
    pub fn new(opening: Script, changing: Script) -> Phased {
        Phased { opening, changing }
    }

    /// The script that puts the content in the tree.
    pub fn opening(&self) -> &Script {
        &self.opening
    }

    /// The script that acts on the rows the host holds by then.
    pub fn changing(&self) -> &Script {
        &self.changing
    }
}

/// What really happened to a tree.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Applied {
    log: Vec<String>,
    places: BTreeSet<String>,
}

impl Applied {
    /// How many steps ran.
    ///
    /// This is the named input a burst's work bound is stated over: ten edits
    /// to one path are ten changes a host may be asked to reconcile, however
    /// many places they name.
    pub fn steps(&self) -> usize {
        self.log.len()
    }

    /// Every distinct place where document content changed.
    ///
    /// This is the named input a spread-out workload's bound is stated over,
    /// and it counts documents rather than directories — see `document_places`
    /// for what each act contributes and why a directory contributes its
    /// contents rather than its name.
    pub fn places(&self) -> &BTreeSet<String> {
        &self.places
    }

    /// What each step that ran said it was doing.
    pub fn log(&self) -> &[String] {
        &self.log
    }
}

impl fmt::Display for Applied {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "{} steps over {} places:",
            self.log.len(),
            self.places.len()
        )?;
        for (position, says) in self.log.iter().enumerate() {
            writeln!(formatter, "  {position}. {says}")?;
        }
        Ok(())
    }
}

/// A step that did not do what it said.
///
/// It carries the sentence rather than the act, because what a reader needs is
/// which part of the workload stopped: a bare `No such file or directory`
/// against a temporary path names nothing a case can act on.
#[derive(Debug)]
pub struct ChurnError {
    pub says: String,
    pub at: PathBuf,
    pub source: io::Error,
}

impl fmt::Display for ChurnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the churn step `{}` failed at {}: {}",
            self.says,
            self.at.display(),
            self.source
        )
    }
}

impl std::error::Error for ChurnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Apply one step, recording it into `applied`.
///
/// The record is written after the act succeeds, so a run's account never
/// claims a change the filesystem refused.
pub fn apply_step(root: &Path, step: &Step, applied: &mut Applied) -> Result<(), ChurnError> {
    let fail = |at: &Path, source: io::Error| ChurnError {
        says: step.says.clone(),
        at: at.to_path_buf(),
        source,
    };
    let changed = document_places(root, &step.act);
    match &step.act {
        Act::Write { at, bytes } => {
            let path = root.join(at);
            create_parent(&path).map_err(|e| fail(&path, e))?;
            fs::write(&path, bytes).map_err(|e| fail(&path, e))?;
        }
        Act::AtomicReplace { at, bytes } => {
            let path = root.join(at);
            create_parent(&path).map_err(|e| fail(&path, e))?;
            // Beside the destination and outside the markdown namespace: an
            // editor's temporary file is in the tree a walk reads, and one
            // named `.md` would be a document of its own for as long as it
            // existed.
            let staging = path.with_extension("md.norn-churn-staging");
            fs::write(&staging, bytes).map_err(|e| fail(&staging, e))?;
            fs::rename(&staging, &path).map_err(|e| fail(&path, e))?;
        }
        Act::Rename { from, to } => {
            let (source, destination) = (root.join(from), root.join(to));
            create_parent(&destination).map_err(|e| fail(&destination, e))?;
            fs::rename(&source, &destination).map_err(|e| fail(&source, e))?;
        }
        Act::Remove { at } => {
            let path = root.join(at);
            fs::remove_file(&path).map_err(|e| fail(&path, e))?;
        }
        Act::CreateDirectory { at } => {
            let path = root.join(at);
            fs::create_dir_all(&path).map_err(|e| fail(&path, e))?;
        }
        Act::RemoveDirectory { at } => {
            let path = root.join(at);
            fs::remove_dir_all(&path).map_err(|e| fail(&path, e))?;
        }
        Act::ReplaceWithDirectory { at } => {
            let path = root.join(at);
            fs::remove_file(&path).map_err(|e| fail(&path, e))?;
            fs::create_dir_all(&path).map_err(|e| fail(&path, e))?;
        }
    }
    applied.log.push(step.says.clone());
    applied.places.extend(changed);
    Ok(())
}

fn create_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent),
        None => Ok(()),
    }
}

/// The seeded stream every byte a script writes comes from.
///
/// One value of state and one step function, so a script built at a seed is the
/// same script every time it is built. Nothing here reads a clock or the
/// operating system's randomness: a workload that drew from either would be a
/// different workload on every run, and a failure would name a tree nobody
/// could write again.
#[derive(Clone, Debug)]
pub struct Ink {
    state: u64,
}

/// The words bodies are drawn from.
///
/// Ordinary lowercase ASCII, so a body is a body: what a document's derived
/// facts are is the text layer's subject, and a workload that wrote adversarial
/// text would be asking that question here instead of the convergence one.
const WORDS: &[&str] = &[
    "alder", "birch", "cedar", "elder", "hazel", "larch", "maple", "olive", "rowan", "spruce",
    "thistle", "willow",
];

impl Ink {
    pub fn new(seed: u64) -> Ink {
        Ink {
            state: seed.wrapping_add(0x9E37_79B9_7F4A_7C15),
        }
    }

    /// The next value of the stream.
    fn next(&mut self) -> u64 {
        // SplitMix64: a whole-period step over 64 bits with a strong finalizer,
        // written out because a seeded stream a test reproduces from a failure
        // message cannot come from a crate whose version decides its output.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn word(&mut self) -> &'static str {
        WORDS[(self.next() % WORDS.len() as u64) as usize]
    }

    /// A document with a frontmatter block, a heading, a link and a tag.
    ///
    /// Every pillar the comparator projects is reachable from one of those, so
    /// a workload writing these documents moves document rows, fact rows and
    /// indexed terms rather than only content hashes.
    pub fn document(&mut self, title: &str) -> Vec<u8> {
        let (one, two, three) = (self.word(), self.word(), self.word());
        format!(
            "---\ntitle: {title}\ntags: [{one}]\n---\n\n# {title}\n\nThe {one} and the {two} \
             stand beside the {three}.\n\nSee [[{two} note]] and #{three}.\n"
        )
        .into_bytes()
    }

    /// A document of `lines` paragraphs, which is how a workload writes one
    /// whose bytes are worth re-reading.
    pub fn long_document(&mut self, title: &str, lines: usize) -> Vec<u8> {
        let mut text = String::from_utf8(self.document(title)).expect("the generator writes text");
        for _ in 0..lines {
            let (one, two) = (self.word(), self.word());
            text.push_str(&format!("\nThe {one} answers the {two}.\n"));
        }
        text.into_bytes()
    }
}

/// Bytes no UTF-8 decoder accepts, in a file a walk reads as a document.
///
/// A place holding these is a place a heal quarantines: no document row, and a
/// finding recorded at the path instead. A script that writes them declares the
/// place with [`Script::without_rows_at`].
pub const UNDECODABLE: &[u8] = b"# a title\n\nthese bytes are not text: \xff\xfe\n";

/// **Family 1. Ordinary creation, modification and deletion across nested
/// directories.**
///
/// The plainest thing a person does to a vault, spread over three levels so
/// that the places a host is asked about are not all siblings: a directory that
/// did not exist gains documents, documents that existed are edited, and a
/// document and a whole directory are removed.
///
/// The changing phase is where the re-derivation and the prune live: every
/// place it names is one the opening phase already put a document at, so a host
/// that settled over the opening is being asked to rewrite rows it holds and to
/// take two of them away.
pub fn ordinary_editing(seed: u64) -> Phased {
    let mut ink = Ink::new(seed);
    let opening = Script::new(
        "ordinary editing across nested directories: the documents that will change",
        vec![
            Step::new(
                "create a document at the vault root",
                Act::Write {
                    at: "churn/root-note.md".to_string(),
                    bytes: ink.document("root note"),
                },
            ),
            Step::new(
                "create a document two directories down",
                Act::Write {
                    at: "churn/inner/deeper/nested-note.md".to_string(),
                    bytes: ink.document("nested note"),
                },
            ),
            Step::new(
                "create a second document beside it",
                Act::Write {
                    at: "churn/inner/deeper/sibling-note.md".to_string(),
                    bytes: ink.document("sibling note"),
                },
            ),
            Step::new(
                "create a document in a directory of its own",
                Act::Write {
                    at: "churn/leaving/departing-note.md".to_string(),
                    bytes: ink.document("departing note"),
                },
            ),
        ],
    );
    let changing = Script::new(
        "ordinary editing across nested directories: rewrites and removals over standing rows",
        vec![
            Step::new(
                "rewrite the root document, which the host holds a row for",
                Act::Write {
                    at: "churn/root-note.md".to_string(),
                    bytes: ink.document("root note, revised"),
                },
            ),
            Step::new(
                "rewrite the nested document, which the host holds a row for",
                Act::Write {
                    at: "churn/inner/deeper/nested-note.md".to_string(),
                    bytes: ink.long_document("nested note, revised", 4),
                },
            ),
            Step::new(
                "delete the document beside the nested one, which the host holds a row for",
                Act::Remove {
                    at: "churn/inner/deeper/sibling-note.md".to_string(),
                },
            ),
            Step::new(
                "delete the directory the departing document sits in, and the document with it",
                Act::RemoveDirectory {
                    at: "churn/leaving".to_string(),
                },
            ),
        ],
    );
    Phased::new(opening, changing)
}

/// **Family 2. Atomic replacement and movement.**
///
/// Content that arrives whole at a name, and names that move between
/// directories. The case flip is [`case_flip`]'s, because what a flip means is
/// the volume's own answer while this workload's claim is the same on every
/// volume.
///
/// Both acts of the changing phase are made against rows the host holds: the
/// replacement lands over a document that already derived, and the move takes a
/// standing row's name away from it.
pub fn atomic_replacement(seed: u64) -> Phased {
    let mut ink = Ink::new(seed);
    let opening = Script::new(
        "atomic replacement and movement: the documents that will change",
        vec![
            Step::new(
                "land a document whole at a name that held nothing",
                Act::AtomicReplace {
                    at: "churn/replaced/landing.md".to_string(),
                    bytes: ink.document("landing"),
                },
            ),
            Step::new(
                "write a document that is about to move",
                Act::Write {
                    at: "churn/replaced/travelling.md".to_string(),
                    bytes: ink.document("travelling"),
                },
            ),
        ],
    );
    let changing = Script::new(
        "atomic replacement and movement: content landing over a row, and a row's name moving",
        vec![
            Step::new(
                "land different content whole over the name the host holds a row for",
                Act::AtomicReplace {
                    at: "churn/replaced/landing.md".to_string(),
                    bytes: ink.long_document("landing, revised", 6),
                },
            ),
            Step::new(
                "move the standing row's document into a directory that did not exist",
                Act::Rename {
                    from: "churn/replaced/travelling.md".to_string(),
                    to: "churn/arrived/travelling.md".to_string(),
                },
            ),
            Step::new(
                "move it again, keeping the directory and changing the name",
                Act::Rename {
                    from: "churn/arrived/travelling.md".to_string(),
                    to: "churn/arrived/settled.md".to_string(),
                },
            ),
        ],
    );
    Phased::new(opening, changing)
}

/// The name the case flip starts at.
pub const FLIPPED_FROM: &str = "churn/replaced/flipping.md";

/// The name the case flip leaves on disk.
pub const FLIPPED_TO: &str = "churn/replaced/FLIPPING.md";

/// **Family 2's normalized-identity half: a name whose case flips.**
///
/// What a flip *is* differs by volume, and the sentence each step carries says
/// which it is. Where two spellings are two places the flip is a move, and the
/// name it arrives at is one nothing stood at. Where they are one place it
/// re-spells that place, and the content landing whole afterwards is what says
/// the one document is still being maintained under its new spelling.
///
/// The acts are the same either way, which is the point: a workload does what a
/// person does, and the volume decides what that means.
///
/// The flip is the changing phase's, so the spelling that moves is a spelling
/// the host is already holding a row at rather than one it has never seen.
pub fn case_flip(seed: u64, folding: Folding) -> Phased {
    let mut ink = Ink::new(seed);
    let opening = Script::new(
        format!("a case flip on {folding}: the document that will flip"),
        vec![Step::new(
            "write the document whose spelling is about to flip case",
            Act::Write {
                at: FLIPPED_FROM.to_string(),
                bytes: ink.document("flipping"),
            },
        )],
    );
    let changing = Script::new(
        format!("a case flip on {folding}"),
        vec![
            Step::new(
                match folding {
                    Folding::Distinct => {
                        "flip the case of the standing row's name, which is a second place on \
                         this volume"
                    }
                    Folding::Folded => {
                        "flip the case of the standing row's name, which re-spells the one place \
                         on this volume"
                    }
                },
                Act::Rename {
                    from: FLIPPED_FROM.to_string(),
                    to: FLIPPED_TO.to_string(),
                },
            ),
            Step::new(
                "land content whole at the flipped spelling",
                Act::AtomicReplace {
                    at: FLIPPED_TO.to_string(),
                    bytes: ink.document("flipping, revised"),
                },
            ),
        ],
    );
    Phased::new(opening, changing)
}

/// How many times the burst rewrites the one path it hammers.
///
/// Enough that a host coalescing nothing would be asked for one increment per
/// edit, and small enough that the default lane pays for it in milliseconds.
pub const BURST_EDITS: usize = 12;

/// **Family 3. Burst and coalescing pressure.**
///
/// One path rewritten [`BURST_EDITS`] times with nothing between the writes,
/// and a wide burst of new documents landing at once. What a host does with
/// either is its own business — coalescing is an optimization and not a
/// contract — and what it converges on is not: the last bytes written to the
/// hammered path are the ones the store ends up holding.
///
/// The opening phase puts the hammered path there and lets the host settle over
/// it, so every one of the burst's edits is a rewrite of a standing row rather
/// than the arrival of a document nothing has seen.
pub fn burst(seed: u64) -> Phased {
    let mut ink = Ink::new(seed);
    let opening = Script::new(
        "a burst against one path and a burst across many: the path to be hammered",
        vec![Step::new(
            "write the path the burst is about to hammer",
            Act::Write {
                at: "churn/burst/hammered.md".to_string(),
                bytes: ink.document("hammered, before the burst"),
            },
        )],
    );
    let mut steps = Vec::new();
    for edit in 0..BURST_EDITS {
        steps.push(Step::new(
            format!("rewrite the standing row's hammered path, edit {edit}"),
            Act::Write {
                at: "churn/burst/hammered.md".to_string(),
                bytes: ink.document(&format!("hammered, edit {edit}")),
            },
        ));
    }
    for wide in 0..6 {
        steps.push(Step::new(
            format!("land document {wide} of a wide burst"),
            Act::Write {
                at: format!("churn/burst/wide/burst-{wide}.md"),
                bytes: ink.document(&format!("burst {wide}")),
            },
        ));
    }
    Phased::new(
        opening,
        Script::new("a burst against one path and a burst across many", steps),
    )
}

/// Where the vault's own schema declaration sits, and the two spellings a
/// validity workload puts there.
///
/// A schema is a vault's own declaration rather than something a script can
/// invent: the bytes that count as one belong to the crate that reads them, so
/// a suite hands them in.
pub struct SchemaGround<'a> {
    /// The vault-relative place the declaration sits at.
    pub at: &'a str,
    /// The declaration the workload replaces the standing one with.
    pub replacement: &'a [u8],
}

/// **Family 4. Transitions between readable, quarantined and valid states, and
/// a schema replacement under them.**
///
/// Four documents, each crossing a boundary in the direction the others do not:
/// one goes from readable to undecodable, one from undecodable back to
/// readable, one crosses the frontmatter read bound in each direction, and one
/// is plain throughout so the workload has a subject nothing happened to. The
/// schema is replaced part-way, which is the act that re-derives what the
/// standing findings were derived under.
///
/// `oversized` is a document whose frontmatter block is past the bound the text
/// layer reads, which — like the schema — is a fact about the layer that reads
/// it rather than one a script can spell.
///
/// The opening phase establishes each of the four states and is settled over,
/// so every transition in the changing phase is a crossing the host has to
/// *unmake* something for: a row it holds goes, a finding it filed clears, a
/// row it holds loses its frontmatter projection. Each phase declares the place
/// that derives no row while it stands, and those are two different places.
pub fn validity_transitions(seed: u64, schema: SchemaGround<'_>, oversized: &[u8]) -> Phased {
    let mut ink = Ink::new(seed);
    let opening = Script::new(
        "transitions between readable, quarantined and valid states: the four states",
        vec![
            Step::new(
                "write a document nothing will happen to",
                Act::Write {
                    at: "churn/states/steady.md".to_string(),
                    bytes: ink.document("steady"),
                },
            ),
            Step::new(
                "write a readable document",
                Act::Write {
                    at: "churn/states/souring.md".to_string(),
                    bytes: ink.document("souring"),
                },
            ),
            Step::new(
                "write a document no decoder accepts",
                Act::Write {
                    at: "churn/states/recovering.md".to_string(),
                    bytes: UNDECODABLE.to_vec(),
                },
            ),
            Step::new(
                "write a document whose frontmatter block is past the read bound",
                Act::Write {
                    at: "churn/states/overlong.md".to_string(),
                    bytes: oversized.to_vec(),
                },
            ),
        ],
    )
    .without_rows_at("churn/states/recovering.md");
    let changing = Script::new(
        "transitions between readable, quarantined and valid states",
        vec![
            Step::new(
                "replace the readable document the host holds a row for with bytes no decoder \
                 accepts",
                Act::Write {
                    at: "churn/states/souring.md".to_string(),
                    bytes: UNDECODABLE.to_vec(),
                },
            ),
            Step::new(
                "replace the undecodable document the host holds no row for with text",
                Act::Write {
                    at: "churn/states/recovering.md".to_string(),
                    bytes: ink.document("recovering"),
                },
            ),
            Step::new(
                "bring the overlong document's frontmatter block back inside the bound",
                Act::Write {
                    at: "churn/states/overlong.md".to_string(),
                    bytes: ink.document("overlong, shortened"),
                },
            ),
            Step::new(
                "put a document past the read bound that was inside it",
                Act::Write {
                    at: "churn/states/steady.md".to_string(),
                    bytes: oversized.to_vec(),
                },
            ),
            Step::new(
                "replace the vault's schema declaration",
                Act::Write {
                    at: schema.at.to_string(),
                    bytes: schema.replacement.to_vec(),
                },
            ),
        ],
    )
    .without_rows_at("churn/states/souring.md");
    Phased::new(opening, changing)
}

/// **Family 5. What external tools do to a vault.**
///
/// Three shapes, and none of them is a person typing. An editor saves by
/// writing a temporary file and renaming it over the document, which is a
/// create and a rename where a naive watcher expects a modification. A
/// synchronization client catching up after a sleep lands a batch of unrelated
/// changes at once — creations, an edit and a deletion together — with nothing
/// between them for a host to react to. And a tool that renames a directory
/// moves every document under it in one act.
///
/// The opening phase is the tree the tools find: a document the editor has
/// already saved once, and the two documents the synchronization client is
/// about to edit and remove. Everything the changing phase does is therefore
/// done to a row the host holds — the second editor save re-derives one, the
/// catch-up edits one and takes one away, and the directory rename moves every
/// one of them at once.
pub fn external_tools(seed: u64) -> Phased {
    let mut ink = Ink::new(seed);
    let opening = Script::new(
        "what external tools do to a vault: the tree they find",
        vec![
            Step::new(
                "the editor opens a new file and saves it",
                Act::AtomicReplace {
                    at: "churn/tools/drafted.md".to_string(),
                    bytes: ink.document("drafted"),
                },
            ),
            Step::new(
                "a document the synchronization client is about to remove",
                Act::Write {
                    at: "churn/tools/synced/withdrawn.md".to_string(),
                    bytes: ink.document("withdrawn"),
                },
            ),
            Step::new(
                "a document the synchronization client is about to edit",
                Act::Write {
                    at: "churn/tools/synced/amended.md".to_string(),
                    bytes: ink.document("amended"),
                },
            ),
        ],
    );
    let mut steps = vec![Step::new(
        "the editor saves the document the host holds a row for again",
        Act::AtomicReplace {
            at: "churn/tools/drafted.md".to_string(),
            bytes: ink.long_document("drafted, second save", 3),
        },
    )];
    // The catch-up batch: everything a sleeping machine missed, landing with
    // nothing between one change and the next.
    for arriving in 0..4 {
        steps.push(Step::new(
            format!("the sleep's catch-up lands new document {arriving}"),
            Act::Write {
                at: format!("churn/tools/synced/arrived-{arriving}.md"),
                bytes: ink.document(&format!("arrived {arriving}")),
            },
        ));
    }
    steps.push(Step::new(
        "the sleep's catch-up edits a document the host holds a row for",
        Act::Write {
            at: "churn/tools/synced/amended.md".to_string(),
            bytes: ink.long_document("amended by the catch-up", 5),
        },
    ));
    steps.push(Step::new(
        "the sleep's catch-up removes a document the host holds a row for",
        Act::Remove {
            at: "churn/tools/synced/withdrawn.md".to_string(),
        },
    ));
    steps.push(Step::new(
        "a tool renames the directory every synced document sits in",
        Act::Rename {
            from: "churn/tools/synced".to_string(),
            to: "churn/tools/reconciled".to_string(),
        },
    ));
    Phased::new(
        opening,
        Script::new("what external tools do to a vault", steps),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree of this module's own, which is a directory the caller removes.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "norn-churn-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("a scratch tree");
        root
    }

    #[test]
    fn one_seed_writes_one_script() {
        assert_eq!(ordinary_editing(11), ordinary_editing(11));
        assert_ne!(ordinary_editing(11), ordinary_editing(12));
    }

    #[test]
    fn a_script_renders_as_its_steps() {
        let rendered = ordinary_editing(3).opening().to_string();
        assert!(
            rendered.contains("workload `ordinary editing"),
            "{rendered}"
        );
        assert!(
            rendered.contains("create a document at the vault root"),
            "{rendered}"
        );
    }

    /// Applying both phases in order leaves the tree the family describes, and
    /// the account of the second phase names the documents it changed.
    #[test]
    fn applying_a_script_writes_and_removes_what_it_says() {
        let root = scratch("ordinary");
        let workload = ordinary_editing(5);
        workload
            .opening()
            .apply(&root)
            .expect("the opening phase applies");
        let applied = workload
            .changing()
            .apply(&root)
            .expect("the changing phase applies");
        assert_eq!(applied.steps(), workload.changing().steps().len());
        assert!(root.join("churn/root-note.md").is_file());
        assert!(root.join("churn/inner/deeper/nested-note.md").is_file());
        assert!(!root.join("churn/inner/deeper/sibling-note.md").exists());
        assert!(!root.join("churn/leaving").exists());
        fs::remove_dir_all(&root).expect("removing the scratch tree");
    }

    /// **A directory act is accounted for by the documents under it.** The
    /// removal names one directory and takes one document with it, and it is
    /// the document that stands in the changed set: a bound counting the
    /// directory as well would be a bound a workload could loosen by naming
    /// directories.
    #[test]
    fn a_directory_act_names_the_documents_under_it_and_not_the_directory() {
        let root = scratch("directory-places");
        let workload = ordinary_editing(5);
        workload
            .opening()
            .apply(&root)
            .expect("the opening phase applies");
        let applied = workload
            .changing()
            .apply(&root)
            .expect("the changing phase applies");
        assert_eq!(
            applied.places().iter().cloned().collect::<Vec<String>>(),
            vec![
                "churn/inner/deeper/nested-note.md".to_string(),
                "churn/inner/deeper/sibling-note.md".to_string(),
                "churn/leaving/departing-note.md".to_string(),
                "churn/root-note.md".to_string(),
            ],
            "{applied}"
        );
        fs::remove_dir_all(&root).expect("removing the scratch tree");
    }

    /// **A directory rename is accounted for at both ends of every document it
    /// moves.** A host owes work where each name emptied and where each filled,
    /// and the directory's own two names are neither.
    #[test]
    fn a_directory_rename_names_both_ends_of_every_document_it_moves() {
        let root = scratch("directory-rename");
        let script = Script::new(
            "a directory of two documents, renamed",
            vec![
                Step::new(
                    "write one",
                    Act::Write {
                        at: "from/one.md".to_string(),
                        bytes: b"# one\n".to_vec(),
                    },
                ),
                Step::new(
                    "write another, a level down",
                    Act::Write {
                        at: "from/under/two.md".to_string(),
                        bytes: b"# two\n".to_vec(),
                    },
                ),
                Step::new(
                    "rename the directory",
                    Act::Rename {
                        from: "from".to_string(),
                        to: "to".to_string(),
                    },
                ),
            ],
        );
        let applied = script.apply(&root).expect("the script applies");
        assert_eq!(
            applied.places().iter().cloned().collect::<Vec<String>>(),
            vec![
                "from/one.md".to_string(),
                "from/under/two.md".to_string(),
                "to/one.md".to_string(),
                "to/under/two.md".to_string(),
            ],
            "{applied}"
        );
        fs::remove_dir_all(&root).expect("removing the scratch tree");
    }

    /// An atomic replacement leaves nothing beside the name it landed at: a
    /// staging file left behind would be a file in the tree a walk reads.
    #[test]
    fn an_atomic_replacement_leaves_no_staging_file() {
        let root = scratch("atomic");
        let workload = atomic_replacement(5);
        workload
            .opening()
            .apply(&root)
            .expect("the opening phase applies");
        workload
            .changing()
            .apply(&root)
            .expect("the changing phase applies");
        let left: Vec<String> = fs::read_dir(root.join("churn/replaced"))
            .expect("the directory the replacement landed in")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into()
            })
            .filter(|name: &String| !name.ends_with(".md"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        fs::remove_dir_all(&root).expect("removing the scratch tree");
    }

    #[test]
    fn a_step_that_cannot_run_names_itself() {
        let root = scratch("failing");
        let script = Script::new(
            "a removal of nothing",
            vec![Step::new(
                "remove a document that is not there",
                Act::Remove {
                    at: "absent.md".to_string(),
                },
            )],
        );
        let failure = script.apply(&root).expect_err("removing nothing fails");
        assert!(
            failure
                .to_string()
                .contains("remove a document that is not there"),
            "{failure}"
        );
        fs::remove_dir_all(&root).expect("removing the scratch tree");
    }

    #[test]
    fn a_script_applied_in_parts_accounts_for_every_part() {
        let root = scratch("parts");
        let workload = burst(9);
        let script = workload.changing();
        let mut applied = Applied::default();
        script
            .apply_range(&root, 0..3, &mut applied)
            .expect("the first part applies");
        assert_eq!(applied.steps(), 3);
        script
            .apply_range(&root, 3..script.steps().len(), &mut applied)
            .expect("the rest applies");
        assert_eq!(applied.steps(), script.steps().len());
        assert!(
            applied
                .to_string()
                .contains("rewrite the standing row's hammered path"),
            "{applied}"
        );
        fs::remove_dir_all(&root).expect("removing the scratch tree");
    }

    #[test]
    fn a_probe_says_what_the_volume_does_with_case_and_leaves_nothing_behind() {
        let root = scratch("folding");
        let answer = folding(&root).expect("the probe runs");
        assert!(matches!(answer, Folding::Folded | Folding::Distinct));
        assert_eq!(
            fs::read_dir(&root).expect("the probe's tree").count(),
            0,
            "the probe left a file behind"
        );
        fs::remove_dir_all(&root).expect("removing the scratch tree");
    }

    /// Each phase declares the place that derives no row while that phase's
    /// tree stands, and the two phases name two different places.
    #[test]
    fn a_quarantining_workload_declares_the_place_that_holds_no_row() {
        let schema = SchemaGround {
            at: ".norn/schema.yaml",
            replacement: b"version: 1\n",
        };
        let workload = validity_transitions(4, schema, b"---\nkey: value\n---\n");
        assert!(
            workload
                .opening()
                .places_without_rows()
                .contains("churn/states/recovering.md"),
            "{:?}",
            workload.opening().places_without_rows()
        );
        assert!(
            workload
                .changing()
                .places_without_rows()
                .contains("churn/states/souring.md"),
            "{:?}",
            workload.changing().places_without_rows()
        );
    }
}
