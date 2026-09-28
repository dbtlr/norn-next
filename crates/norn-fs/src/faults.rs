//! Making a write fail without the environment cooperating.
//!
//! Two of this kernel's contract claims are about conditions a test cannot
//! arrange: a disk that fills between the shadow's first byte and its last, and
//! an fsync that fails after a rename has already published a name. Both are
//! real, both are the reason the code has the shape it has, and neither is
//! reachable by writing files into a temporary directory.
//!
//! So each stage of the protocol asks [`Faults`] whether it is the stage that
//! fails. [`Faults::entry`] is what the public entry points outside a
//! publication pass, and [`Faults::publication`] what each publication passes;
//! the in-crate suite passes a stage and an [`Answer`] and reads what the
//! protocol did with it.
//!
//! The same shape carries the other half of what a test cannot arrange: a
//! foreign writer landing inside a window one call wide. [`Window`] names those
//! windows, and a disturbance is handed the window it is standing in.
//!
//! **The seam is deliberately small.** It names *where* a write can be made to
//! fail, in *which* publication, and *how* — an error, a full disk, the end of
//! the process, or a foreign writer's act on the target — and never what the
//! protocol does next, which is the code under test.
//!
//! # Reaching it from outside
//!
//! One stage of the widening is taken and no more: under the `induced-failure`
//! feature, the write protocol's five public entry points —
//! [`stage`](crate::stage), [`publish`](crate::publish),
//! [`confirm_landed`](crate::confirm_landed), [`discard`](crate::discard) and
//! [`remove_empty_folders`](crate::remove_empty_folders) — arm themselves from this process's environment
//! rather than passing [`Faults::NONE`]. That is what a lockdown suite's
//! process-death bars need and what nothing else can give them — a stage whose
//! required outcome is "this process does not survive here" cannot be reached
//! by a caller that has to return to make its assertion, so the arm is read by
//! the child that dies and the assertion is made by the parent that spawned it.
//!
//! Two variables carry it, both read once:
//!
//! - `NORN_FS_ARMED_STAGES` — the arm, as comma-separated `selector=answer`
//!   pairs. A selector is a stage, or a stage and a publication ordinal as
//!   `stage@N`:
//!
//!   ```text
//!   arm      = pair { "," pair }
//!   pair     = ( counted [ "@" N ] | uncounted ) "=" ( "fails" | "full-disk" | "ends" ) | "foreign@" N "=" ( "edit" | "remove" | "take" )
//!   counted  = "swap" | "unlink" | "respell" | "mkdir" | "parent-sync" | "cleanup"
//!   uncounted = "stage-create" | "stage-write" | "stage-sync" | "rmdir"
//!   ```
//!
//!   Staging's three stages and `rmdir` take no ordinal, because no counted
//!   call reaches them: staging comes before any publication, and a folder is
//!   removed only while emptying folders. `N` counts from 1, one per call of
//!   [`publish`](crate::publish) across every kind in this process;
//!   [`confirm_landed`](crate::confirm_landed),
//!   [`remove_empty_folders`](crate::remove_empty_folders) and
//!   [`discard`](crate::discard) do not count, so an arm with an ordinal never
//!   fires in them. A stage with no ordinal fires wherever it is reached, every
//!   time. One stage may be armed at several ordinals; the same selector twice
//!   is a mistake, and so is one stage armed both bare and by ordinal, since
//!   the bare arm would answer first everywhere. The `foreign` stage is a foreign writer acting on the target
//!   inside the `N`th publication, after the root is checked and before the
//!   target is read again: `edit` writes fixed foreign bytes at the target's
//!   name, `remove` removes the target, and `take` claims a create's name —
//!   against any other kind, `take` is recorded and does nothing. A pair this
//!   module cannot read is a mistake in the harness rather than a stage nothing
//!   is armed at, so it panics.
//! - `NORN_FS_ARM_HITS` — a file each fired arm appends one record to before it
//!   answers — `seam=norn-fs/write stage=<stage> ordinal=<N or -> path=<the
//!   vault-relative path the stage acts on, percent-encoded> answer=<answer>`
//!   — so a parent
//!   reads *which* checkpoint the protocol reached rather
//!   than inferring it from what the child left behind. Neutering a stage's
//!   `check` call takes its record away, which is what makes a bypassed hook
//!   fail the case it was supposed to carry.
//!
//! Nothing outside this crate arms anything without the feature, and a shipped
//! build has no reader for either variable.
//!
//! Two sibling seams carry the effect surfaces this one does not: the watcher's,
//! widened once at watch establishment, and the walk's, widened once at a walk's
//! construction. Both stand under the same feature and append to the same record
//! file, under the `norn-fs/watch` and `norn-fs/walk` seam names. Each seam
//! answers at its own boundary and at no other, which is what the `seam` field in
//! a record says.
//!
//! **What the three share, they share from here.** Which stages a seam holds and
//! what each answer does are its own; the grammar an arm is spelled in, the
//! refusal of a spelling that cannot be read, the record file and the writing of
//! a record are one discipline across all three, and a second copy of any of them
//! is a copy that drifts. [`read_armed_pairs`], [`refuse_a_stage_armed_twice`],
//! [`armed_hits`] and [`append_record`] are that discipline.

use std::io;

/// The environment variable naming the stages this process is armed at.
#[cfg(any(test, feature = "induced-failure"))]
pub(crate) const ARMED_STAGES: &str = "NORN_FS_ARMED_STAGES";

/// The environment variable naming the file fired arms record themselves in.
#[cfg(feature = "induced-failure")]
pub(crate) const ARM_HITS: &str = "NORN_FS_ARM_HITS";

/// The seam a record written by the write protocol names itself under.
#[cfg(feature = "induced-failure")]
const SEAM: &str = "norn-fs/write";

/// Append one record of a fired arm to the file `hits`.
///
/// All three fault seams write through here, because the discipline is one and a
/// second copy of it is a copy that drifts: one line per firing, and no
/// buffering — a record still in this process's memory when the process ends is
/// a record the harness never reads — so it opens, writes, syncs and closes
/// each time.
///
/// **What a failure means is the caller's, and the callers differ.** The
/// write protocol's arm often fires in a process that is about to abort, where
/// best effort is the only kind of effort there is; the watcher's and the walk's
/// fire in a process that lives to be asked, where a named file that cannot be
/// written would leave a parent reading silence as a boundary that was never
/// reached. [`record_or_abort`] is that second reading.
#[cfg(any(test, feature = "induced-failure"))]
pub(crate) fn append_record(
    hits: &std::path::Path,
    seam: &str,
    stage: &str,
    answer: &str,
) -> io::Result<()> {
    append_line(hits, &format!("seam={seam} stage={stage} answer={answer}"))
}

/// Append one record line, spelled by the seam that fired, to the file `hits`.
///
/// The write seam's records carry more fields than the other two seams', so
/// the line is its own; the opening, the appending and the sync are the
/// discipline, and they are here once.
#[cfg(any(test, feature = "induced-failure"))]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The arm's own record file, outside the vault.
pub(crate) fn append_line(hits: &std::path::Path, record: &str) -> io::Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(hits)?;
    file.write_all(format!("{record}\n").as_bytes())?;
    file.sync_all()
}

/// Append one record of a fired arm, or end the process saying it could not.
///
/// The reading the seams that outlive their arms share. A harness that named no
/// record file wants none. A harness that named one wants every firing in it, so
/// a file that cannot be written ends the process rather than leaving a parent
/// to read the silence as a boundary the code never reached.
///
/// **Deliberately an abort rather than a panic.** One of the boundaries this
/// carries answers on a backend's own delivery thread, where an unwind ends that
/// thread and leaves the subscription standing — which is the same silence,
/// reached a different way. Nothing here is recoverable in any case: the harness
/// named a file this process cannot write, and every later arm would meet it
/// too. The reason goes to standard error first, because an abort says nothing
/// on its own.
#[cfg(any(test, feature = "induced-failure"))]
pub(crate) fn record_or_abort(
    hits: Option<&std::path::Path>,
    seam: &str,
    stage: &str,
    answer: &str,
) {
    let Some(hits) = hits else {
        return;
    };
    if let Err(error) = append_record(hits, seam, stage, answer) {
        eprintln!(
            "norn-fs: the {stage} arm could not record itself in {}: {error}",
            hits.display()
        );
        std::process::abort();
    }
}

/// The file fired arms record themselves in, where this process named one.
///
/// One reading for all three seams, held after the first: every seam appends to
/// the same file under its own name, so three readings of one variable would be
/// three chances for them to disagree about whether a harness named it.
#[cfg(feature = "induced-failure")]
pub(crate) fn armed_hits() -> Option<&'static std::path::PathBuf> {
    static HITS: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    HITS.get_or_init(|| std::env::var_os(ARM_HITS).map(std::path::PathBuf::from))
        .as_ref()
}

/// Refuse an arm that names one stage twice.
///
/// **The one mistake in an arm that would otherwise pass silently.** Every seam
/// here reads a stage's first answer, so a second pair a harness spelled would
/// simply not happen and the case would report on a condition it never met. The
/// grammar is one across the seams, so the refusal is too: a spelling that ends
/// one process saying so cannot quietly arm half of itself in another.
#[cfg(any(test, feature = "induced-failure"))]
pub(crate) fn refuse_a_stage_armed_twice<S: PartialEq, A>(
    armed: &[(S, A)],
    name: impl Fn(&S) -> String,
    source: &str,
) {
    for (index, (stage, _)) in armed.iter().enumerate() {
        assert!(
            !armed[..index].iter().any(|(earlier, _)| earlier == stage),
            "the {} stage is armed twice in {source}",
            name(stage)
        );
    }
}

/// Read one arm's `stage=answer` pairs, and refuse a spelling the seam reading
/// it cannot answer.
///
/// The grammar all three seams are armed under, in one place: comma-separated
/// pairs, each naming a stage of that seam and an answer it carries. A
/// misspelled arm that quietly armed nothing would pass every bar it was
/// supposed to carry, so an unreadable pair ends the process saying so. What
/// each name spells is the seam's own, which is what the two lookups carry.
#[cfg(any(test, feature = "induced-failure"))]
pub(crate) fn read_armed_pairs<S: PartialEq, A>(
    spelling: &str,
    source: &str,
    stage_named: impl Fn(&str) -> Option<S>,
    answer_named: impl Fn(&str) -> Option<A>,
    stage_name: impl Fn(&S) -> String,
) -> Vec<(S, A)> {
    let armed: Vec<(S, A)> = spelling
        .split(',')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (stage, answer) = pair
                .split_once('=')
                .unwrap_or_else(|| panic!("`{pair}` in {source} is not `stage=answer`"));
            let stage = stage_named(stage)
                .unwrap_or_else(|| panic!("`{stage}` in {source} names no stage"));
            let answer = answer_named(answer)
                .unwrap_or_else(|| panic!("`{answer}` in {source} names no answer"));
            (stage, answer)
        })
        .collect();
    refuse_a_stage_armed_twice(&armed, stage_name, source);
    armed
}

/// A point in the write protocol that can be made to fail.
///
/// The stages are the ones whose failure has a *different* required outcome,
/// which is what makes each of them worth naming, and each position has its
/// own. A failure while staging, or in publication before the publication act,
/// refuses and leaves the target alone; a failure after the act has already
/// changed a name and must never read as a write that did not happen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Opening a shadow, while staging.
    ShadowCreate,
    /// Putting the content into it.
    ShadowWrite,
    /// Getting those bytes onto the disk, before any name points at them.
    ShadowSync,
    /// The rename that publishes a create or a replace — and a respell's
    /// replace under its old spelling.
    Swap,
    /// The unlink that publishes a removal.
    Unlink,
    /// The rename that gives a respell its new spelling.
    Respell,
    /// Making one of a create's missing folders.
    Mkdir,
    /// Removing one empty folder while emptying upward.
    Rmdir,
    /// A folder's fsync after a publication, which happens after the name is
    /// already live — and when a landing is confirmed.
    ParentSync,
    /// Removing a shadow or a made folder a refusal or a discard abandons.
    /// Injecting here stands in for a removal the filesystem refused — the one
    /// condition whose required behavior is to change nothing at all about the
    /// outcome.
    Cleanup,
    /// A foreign writer acting on the target inside a publication, after the
    /// root is checked and before anything is read again. Armed only with an
    /// ordinal, and answered only by a [`ForeignAct`].
    Foreign,
}

// The stage vocabulary — the roster and the names — is what a harness arms and
// what a record names, so it compiles where something reads it and nowhere
// else. It still belongs to the seam rather than to the suite: the names are
// what the widened half is stated over, and a build that carried a different
// set would be a different seam. The variants themselves carry no such gate:
// the protocol names one at every stage it checks, in every build.
impl Stage {
    /// Every stage, staging's first and then publication's.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const ALL: [Stage; 11] = [
        Stage::ShadowCreate,
        Stage::ShadowWrite,
        Stage::ShadowSync,
        Stage::Swap,
        Stage::Unlink,
        Stage::Respell,
        Stage::Mkdir,
        Stage::Rmdir,
        Stage::ParentSync,
        Stage::Cleanup,
        Stage::Foreign,
    ];

    /// The name a harness arms this stage under, which is also the name a
    /// record of it carries.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Stage::ShadowCreate => "stage-create",
            Stage::ShadowWrite => "stage-write",
            Stage::ShadowSync => "stage-sync",
            Stage::Swap => "swap",
            Stage::Unlink => "unlink",
            Stage::Respell => "respell",
            Stage::Mkdir => "mkdir",
            Stage::Rmdir => "rmdir",
            Stage::ParentSync => "parent-sync",
            Stage::Cleanup => "cleanup",
            Stage::Foreign => "foreign",
        }
    }

    /// Whether an arm may select this stage by publication ordinal.
    ///
    /// Only a stage some publication reaches can be counted to. Staging comes
    /// before any publication, and a folder's removal is reached only while
    /// emptying folders, which no count covers; an ordinal on either would
    /// name a firing that can never come.
    #[cfg(any(test, feature = "induced-failure"))]
    const fn takes_an_ordinal(self) -> bool {
        !matches!(
            self,
            Stage::ShadowCreate | Stage::ShadowWrite | Stage::ShadowSync | Stage::Rmdir
        )
    }
}

/// What an armed stage does when the protocol reaches it.
///
/// The first three are the three shapes a write's environment fails in, and
/// each has a different required outcome: an error the protocol refuses on, a
/// disk with no room left, and a machine that stops between two system calls.
/// Naming them apart is what lets one arm say `ENOSPC` — which has no
/// [`io::ErrorKind`](std::io::ErrorKind) of its own, and which a caller reads
/// off the refusal as an error number — and another say nothing at all,
/// because the process it was armed in does not reach a return. The fourth is
/// the foreign stage's alone.
// The allow stays at the enum rather than moving onto its items: nothing in a
// build that arms nothing constructs an answer at all, and a variant nobody
// names is what the lint reads as dead. The variants are the seam's vocabulary
// even so — an unarmed build has to hold the same set, or the arm a harness
// spells means something different from the arm this seam answers.
#[cfg_attr(not(feature = "induced-failure"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Answer {
    /// The stage meets this error.
    Fails(io::ErrorKind),
    /// The stage meets a full disk: `ENOSPC`, carrying the error number, which
    /// is the only way a refusal can be told apart from any other write failure.
    MeetsAFullDisk,
    /// The process ends at the stage, before the stage's own work runs. No
    /// unwinding, no destructor, no shadow removed on the way out — which is
    /// what a machine losing power between two system calls leaves behind, and
    /// the only thing the process-death bars can be stated over.
    Ends,
    /// A foreign writer acts on the target.
    Foreign(ForeignAct),
}

/// What the foreign stage's writer does to a publication's target, through the
/// target's folder handle.
#[cfg_attr(not(feature = "induced-failure"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForeignAct {
    /// Write [`FOREIGN_BYTES`] at the target's name — over the target, or at a
    /// create's name.
    Edit,
    /// Remove the target.
    Remove,
    /// Claim a create's name with [`FOREIGN_BYTES`]. Against any other kind it
    /// is recorded and does nothing: only a create has a name to take.
    Take,
}

/// What the foreign stage's writer puts at a name.
#[cfg_attr(not(any(test, feature = "induced-failure")), allow(dead_code))]
pub(crate) const FOREIGN_BYTES: &[u8] = b"bytes a foreign writer put here\n";

impl Answer {
    /// The name a harness arms this answer under.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Answer::Fails(_) => "fails",
            Answer::MeetsAFullDisk => "full-disk",
            Answer::Ends => "ends",
            Answer::Foreign(ForeignAct::Edit) => "edit",
            Answer::Foreign(ForeignAct::Remove) => "remove",
            Answer::Foreign(ForeignAct::Take) => "take",
        }
    }

    /// The answer `name` spells, or nothing where it spells none.
    #[cfg(any(test, feature = "induced-failure"))]
    fn named(name: &str) -> Option<Answer> {
        match name {
            "fails" => Some(Answer::Fails(io::ErrorKind::Other)),
            "full-disk" => Some(Answer::MeetsAFullDisk),
            "ends" => Some(Answer::Ends),
            "edit" => Some(Answer::Foreign(ForeignAct::Edit)),
            "remove" => Some(Answer::Foreign(ForeignAct::Remove)),
            "take" => Some(Answer::Foreign(ForeignAct::Take)),
            _ => None,
        }
    }

    /// The error this answer meets a stage with.
    fn error(self, stage: Stage) -> io::Error {
        match self {
            Answer::Fails(kind) => {
                io::Error::new(kind, format!("injected failure at the {stage:?} stage"))
            }
            Answer::MeetsAFullDisk => io::Error::from_raw_os_error(libc::ENOSPC),
            // Nothing reaches these: `check` ends the process before it asks for
            // an error to return, and a foreign act is never a check's answer.
            Answer::Ends => io::Error::other("the process was armed to end here"),
            Answer::Foreign(_) => io::Error::other("a foreign act answers no check"),
        }
    }
}

/// Which stage an arm names, and at which publication, if it names one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Selector {
    pub(crate) stage: Stage,
    /// The 1-based publication the arm fires in, or `None` for every one and
    /// for the stages no publication reaches.
    pub(crate) ordinal: Option<u32>,
}

impl Selector {
    /// How the selector is spelled: `stage` or `stage@N`.
    #[cfg(any(test, feature = "induced-failure"))]
    fn spelled(&self) -> String {
        match self.ordinal {
            Some(ordinal) => format!("{}@{ordinal}", self.stage.name()),
            None => self.stage.name().to_string(),
        }
    }

    /// The selector `spelling` names, or nothing where it names none. A
    /// staging stage with an ordinal, and a foreign stage without one, name
    /// none.
    #[cfg(any(test, feature = "induced-failure"))]
    fn named(spelling: &str) -> Option<Selector> {
        let (stage, ordinal) = match spelling.split_once('@') {
            Some((stage, ordinal)) => {
                let ordinal: u32 = ordinal.parse().ok().filter(|ordinal| *ordinal > 0)?;
                (stage, Some(ordinal))
            }
            None => (spelling, None),
        };
        let stage = Stage::ALL.into_iter().find(|it| it.name() == stage)?;
        match (stage, ordinal) {
            (stage, Some(_)) if !stage.takes_an_ordinal() => None,
            (Stage::Foreign, None) => None,
            _ => Some(Selector { stage, ordinal }),
        }
    }
}

/// A point in the protocol where a foreign actor's act can be made to land.
///
/// Each window is one call wide in a real run, which is why a test arranges it
/// rather than races for it: the defense would otherwise be asserted instead of
/// checked. The windows are the ones where something outside this process can
/// make a statement the protocol is about to make untrue.
///
/// What lands *between* staging and publication needs no window: a case acts
/// between its two calls. These are the windows inside publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Window {
    /// A target's bytes have been hashed through the handle that read them,
    /// and the name has not been confirmed to still mean that file.
    Verifying,
    /// Every check has passed and the publication act is next.
    Publishing,
    /// A create has made one of its missing folders and has not opened it.
    FolderMade,
    /// A respell's first step is done and its rename has not read the target
    /// again.
    Respelling,
}

/// Which stages of a write fail, and how, and which publication this is.
///
/// A list rather than one entry, because two of the claims are about what
/// happens when a *second* thing goes wrong: a swap that fails and then a
/// cleanup that cannot happen either is the condition under which a shadow
/// leaks, and it is unreachable if only one stage at a time can be made to fail.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Faults {
    injected: &'static [(Selector, Answer)],
    /// The 1-based ordinal of the publication these faults were taken for, or
    /// `None` outside one.
    publication: Option<u32>,
}

impl Faults {
    /// A write that fails only where the machine makes it fail.
    #[cfg(any(test, not(feature = "induced-failure")))]
    pub(crate) const NONE: Faults = Faults {
        injected: &[],
        publication: None,
    };

    /// A write that answers each named stage the named way, whatever
    /// publication it is in.
    #[cfg(test)]
    pub(crate) fn at(injected: &'static [(Stage, Answer)]) -> Faults {
        let selected: Vec<(Selector, Answer)> = injected
            .iter()
            .map(|(stage, answer)| {
                (
                    Selector {
                        stage: *stage,
                        ordinal: None,
                    },
                    *answer,
                )
            })
            .collect();
        Faults {
            injected: selected.leak(),
            publication: None,
        }
    }

    /// The same arms, in the `ordinal`th publication.
    #[cfg(test)]
    pub(crate) fn selected(injected: &'static [(Selector, Answer)], ordinal: u32) -> Faults {
        Faults {
            injected,
            publication: Some(ordinal),
        }
    }

    /// What a public entry point outside a publication passes.
    ///
    /// Without the `induced-failure` feature this is [`Faults::NONE`] and the
    /// entry point asks one comparison against an empty list. With it, the
    /// answer is whatever this process was started armed with — read once, and
    /// empty in every process that armed nothing, which is every process that
    /// is not a lockdown suite's child. An arm with an ordinal never fires here:
    /// only [`Faults::publication`] counts.
    pub(crate) fn entry() -> Faults {
        #[cfg(feature = "induced-failure")]
        {
            Faults {
                injected: armed::stages(),
                publication: None,
            }
        }
        #[cfg(not(feature = "induced-failure"))]
        {
            Faults::NONE
        }
    }

    /// What one call of [`publish`](crate::publish) passes: the process's arm,
    /// and the ordinal of this publication.
    ///
    /// **One count per `publish` call, across every kind, from 1.** Staging,
    /// [`confirm_landed`](crate::confirm_landed),
    /// [`remove_empty_folders`](crate::remove_empty_folders) and
    /// [`discard`](crate::discard) are not publications and do not count.
    pub(crate) fn publication() -> Faults {
        #[cfg(feature = "induced-failure")]
        {
            Faults {
                injected: armed::stages(),
                publication: Some(armed::next_publication()),
            }
        }
        #[cfg(not(feature = "induced-failure"))]
        {
            Faults::NONE
        }
    }

    /// The answer armed for `stage` here: an arm with no ordinal answers
    /// wherever the stage is reached, and one with an ordinal only in that
    /// publication.
    fn armed(&self, stage: Stage) -> Option<Answer> {
        self.injected
            .iter()
            .find(|(selector, _)| {
                selector.stage == stage
                    && selector
                        .ordinal
                        .is_none_or(|ordinal| Some(ordinal) == self.publication)
            })
            .map(|(_, answer)| *answer)
    }

    /// The error `stage` is supposed to meet at `path`, if it is armed here.
    ///
    /// A stage the arm names is recorded before it is answered, so a record
    /// stands for every checkpoint the protocol actually reached — including the
    /// one the process does not return from.
    pub(crate) fn check(&self, stage: Stage, path: &std::path::Path) -> io::Result<()> {
        let Some(answer) = self.armed(stage) else {
            return Ok(());
        };
        self.record(stage, path, answer);
        if answer == Answer::Ends {
            // Deliberately an abort rather than a panic: a panic unwinds, and an
            // unwind runs the removals that make a half-published write tidy
            // again. The tidy end is not the one this bar is about.
            std::process::abort();
        }
        Err(answer.error(stage))
    }

    /// The act the foreign stage's writer makes at `path` here, if it is armed.
    #[cfg_attr(not(any(test, feature = "induced-failure")), allow(dead_code))]
    pub(crate) fn foreign(&self, path: &std::path::Path) -> Option<ForeignAct> {
        let answer = self.armed(Stage::Foreign)?;
        self.record(Stage::Foreign, path, answer);
        match answer {
            Answer::Foreign(act) => Some(act),
            // The grammar gives the foreign stage no other answer.
            _ => None,
        }
    }

    #[cfg_attr(not(feature = "induced-failure"), allow(unused_variables))]
    fn record(&self, stage: Stage, path: &std::path::Path, answer: Answer) {
        #[cfg(feature = "induced-failure")]
        armed::record(stage, self.publication, path, answer);
    }
}

/// The arm this process was started under.
///
/// Both readings happen once and are then held: a write asks the seam at every
/// position and a heal-scale run asks it a great many times, so re-reading the
/// environment per stage would make the feature's cost a function of how much
/// is written.
#[cfg(feature = "induced-failure")]
mod armed {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{ARMED_STAGES, Answer, Selector, Stage};

    /// The publications this process has made.
    static PUBLICATIONS: AtomicU32 = AtomicU32::new(0);

    /// The ordinal of the publication starting now.
    pub(super) fn next_publication() -> u32 {
        PUBLICATIONS.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The arms this process is armed with, in the order they were named.
    pub(super) fn stages() -> &'static [(Selector, Answer)] {
        static STAGES: OnceLock<Vec<(Selector, Answer)>> = OnceLock::new();
        STAGES.get_or_init(|| match std::env::var_os(ARMED_STAGES) {
            None => Vec::new(),
            Some(spelling) => super::parse(
                spelling
                    .to_str()
                    .unwrap_or_else(|| panic!("{ARMED_STAGES} is not UTF-8")),
            ),
        })
    }

    /// Append one record saying which checkpoint fired, in which publication,
    /// about which path, and how it answered.
    ///
    /// Best effort by construction: the process this runs in is often about to
    /// end, and a harness that armed no record file wants none. A record that
    /// could not be written is dropped rather than raised, because raising it
    /// would replace the death this arm exists to cause with a different one.
    pub(super) fn record(
        stage: Stage,
        publication: Option<u32>,
        path: &std::path::Path,
        answer: Answer,
    ) {
        let Some(hits) = super::armed_hits() else {
            return;
        };
        let ordinal = publication.map_or_else(|| "-".to_string(), |ordinal| ordinal.to_string());
        let _ = super::append_line(
            hits,
            &format!(
                "seam={} stage={} ordinal={ordinal} path={} answer={}",
                super::SEAM,
                stage.name(),
                super::encoded(path),
                answer.name()
            ),
        );
    }
}

/// A path as a record field spells it: every byte outside `A–Z a–z 0–9 - . _ ~
/// /` as `%` and two upper-case hex digits.
///
/// A record is space-separated `key=value` fields, one per line, so a path
/// holding a space, an `=` or a newline would otherwise forge a field or a
/// record. The escape is percent-encoding's, `%` itself included, so a reader
/// can take the value back byte for byte.
#[cfg(any(test, feature = "induced-failure"))]
pub(crate) fn encoded(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                char::from(*byte).to_string()
            }
            byte => format!("%{byte:02X}"),
        })
        .collect()
}

/// Read the write seam's `selector=answer` pairs through the grammar all three
/// seams share, and refuse the pairings this seam alone can be given wrong: a
/// foreign act at any stage but the foreign one, and a failure at the foreign
/// one.
#[cfg(any(test, feature = "induced-failure"))]
fn parse(spelling: &str) -> Vec<(Selector, Answer)> {
    let armed = read_armed_pairs(
        spelling,
        ARMED_STAGES,
        Selector::named,
        Answer::named,
        Selector::spelled,
    );
    for (index, (selector, _)) in armed.iter().enumerate() {
        // A stage armed bare fires in every publication, so an ordinal arm of
        // the same stage beside it would never be the one that answers.
        assert!(
            !armed[..index]
                .iter()
                .any(|(earlier, _)| earlier.stage == selector.stage
                    && earlier.ordinal.is_none() != selector.ordinal.is_none()),
            "the {} stage is armed both bare and by ordinal in {ARMED_STAGES}",
            selector.stage.name()
        );
    }
    for (selector, answer) in &armed {
        assert_eq!(
            selector.stage == Stage::Foreign,
            matches!(answer, Answer::Foreign(_)),
            "the {} stage in {ARMED_STAGES} answers no `{}`",
            selector.spelled(),
            answer.name()
        );
    }
    armed
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// The seam asks nothing when nothing is injected. A default that failed
    /// somewhere would make every ordinary write a test of this module.
    #[test]
    fn no_fault_lets_every_stage_through() {
        for stage in Stage::ALL {
            Faults::NONE
                .check(stage, Path::new("note.md"))
                .expect("no injected failure");
        }
    }

    /// One stage fails and the others do not, so a bar reaches the stage it
    /// names rather than the first one the protocol happens to run.
    #[test]
    fn an_injected_fault_fires_at_one_stage_only() {
        let faults = Faults::at(&[(Stage::Swap, Answer::Fails(io::ErrorKind::PermissionDenied))]);
        let error = faults
            .check(Stage::Swap, Path::new("note.md"))
            .expect_err("the injected stage");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Swap"), "{error}");
        for other in Stage::ALL.into_iter().filter(|it| *it != Stage::Swap) {
            faults
                .check(other, Path::new("note.md"))
                .expect("a stage nothing injected");
        }
    }

    /// A full disk carries the error number a caller classifies on. There is no
    /// [`io::ErrorKind`] for it, so a refusal that lost the number is a refusal
    /// nothing can tell apart from any other failed write.
    #[test]
    fn a_full_disk_carries_the_error_number_that_names_it() {
        let faults = Faults::at(&[(Stage::ShadowWrite, Answer::MeetsAFullDisk)]);
        let error = faults
            .check(Stage::ShadowWrite, Path::new("note.md"))
            .expect_err("the injected stage");
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
    }

    /// **The bar on the ordinal.** An arm with an ordinal fires in that
    /// publication and in no other, and outside every publication not at all;
    /// an arm without one fires in each.
    #[test]
    fn an_ordinal_selects_one_publication() {
        static ARMED: [(Selector, Answer); 2] = [
            (
                Selector {
                    stage: Stage::Swap,
                    ordinal: Some(2),
                },
                Answer::MeetsAFullDisk,
            ),
            (
                Selector {
                    stage: Stage::Unlink,
                    ordinal: None,
                },
                Answer::MeetsAFullDisk,
            ),
        ];
        let path = Path::new("note.md");
        for (publication, swap_fires) in [(1, false), (2, true), (3, false)] {
            let faults = Faults::selected(&ARMED, publication);
            assert_eq!(
                faults.check(Stage::Swap, path).is_err(),
                swap_fires,
                "{publication}"
            );
            assert!(faults.check(Stage::Unlink, path).is_err(), "{publication}");
        }
        let outside = Faults {
            injected: &ARMED,
            publication: None,
        };
        outside
            .check(Stage::Swap, path)
            .expect("an ordinal outside any publication");
    }

    /// The foreign stage answers with its act and fires only where selected.
    #[test]
    fn the_foreign_stage_answers_with_its_act() {
        static ARMED: [(Selector, Answer); 1] = [(
            Selector {
                stage: Stage::Foreign,
                ordinal: Some(1),
            },
            Answer::Foreign(ForeignAct::Take),
        )];
        let path = Path::new("note.md");
        assert_eq!(
            Faults::selected(&ARMED, 1).foreign(path),
            Some(ForeignAct::Take)
        );
        assert_eq!(Faults::selected(&ARMED, 2).foreign(path), None);
    }

    /// **The grammar.** Every stage reads bare and every publication stage
    /// with an ordinal; one stage may be armed at several ordinals.
    #[test]
    fn the_arm_grammar_reads_selectors_and_ordinals() {
        assert_eq!(
            parse("swap@2=ends,swap@3=fails,unlink=full-disk,stage-write=fails,foreign@1=take"),
            vec![
                (
                    Selector {
                        stage: Stage::Swap,
                        ordinal: Some(2)
                    },
                    Answer::Ends
                ),
                (
                    Selector {
                        stage: Stage::Swap,
                        ordinal: Some(3)
                    },
                    Answer::Fails(io::ErrorKind::Other)
                ),
                (
                    Selector {
                        stage: Stage::Unlink,
                        ordinal: None
                    },
                    Answer::MeetsAFullDisk
                ),
                (
                    Selector {
                        stage: Stage::ShadowWrite,
                        ordinal: None
                    },
                    Answer::Fails(io::ErrorKind::Other)
                ),
                (
                    Selector {
                        stage: Stage::Foreign,
                        ordinal: Some(1)
                    },
                    Answer::Foreign(ForeignAct::Take)
                ),
            ]
        );
        for stage in Stage::ALL
            .into_iter()
            .filter(|stage| *stage != Stage::Foreign)
        {
            if stage.takes_an_ordinal() {
                assert_eq!(parse(&format!("{}@3=ends", stage.name())).len(), 1);
            }
            assert_eq!(parse(&format!("{}=ends", stage.name())).len(), 1);
        }
    }

    /// **A spelling this seam cannot read ends the process saying so.** This
    /// seam answers a selector's first pair, so a second one would silently not
    /// happen and the case would report on a condition it never met; an
    /// ordinal on a staging stage, a foreign stage without one, and an answer
    /// the stage does not carry are each a mistake of the same kind.
    #[test]
    fn an_unreadable_pair_refuses_rather_than_arming_half_of_itself() {
        for spelling in [
            "swap",
            "swop=fails",
            "swap=melts",
            "create=fails",
            "swap=fails,swap=ends",
            "swap@2=fails,swap@2=ends",
            "stage-write@2=fails",
            "stage-create@1=ends",
            "swap@0=ends",
            "swap@x=ends",
            "swap@=ends",
            "foreign=edit",
            "foreign@1=ends",
            "swap@1=edit",
            "unlink=take",
            "swap=ends,swap@2=fails",
            "swap@2=fails,swap=ends",
            "rmdir@1=fails",
        ] {
            assert!(
                std::panic::catch_unwind(|| parse(spelling)).is_err(),
                "`{spelling}` was read as an arm"
            );
        }
    }

    /// **A record's path cannot forge a field or a record.** Every byte a
    /// record's grammar gives meaning to is escaped, and the escape itself.
    #[test]
    fn a_record_path_is_percent_encoded() {
        assert_eq!(
            encoded(std::path::Path::new("a b/c=d\n%e.md")),
            "a%20b/c%3Dd%0A%25e.md"
        );
        assert_eq!(
            encoded(std::path::Path::new("notes/note-1_x.md")),
            "notes/note-1_x.md"
        );
    }

    /// Every stage and every answer a harness arms round-trips through the name
    /// it is armed under, so a widened seam and the suite arming it cannot drift
    /// into naming different things.
    #[test]
    fn every_stage_and_answer_has_one_name() {
        let names: std::collections::BTreeSet<&str> =
            Stage::ALL.iter().map(|stage| stage.name()).collect();
        assert_eq!(names.len(), Stage::ALL.len());
        for answer in [
            Answer::Fails(io::ErrorKind::Other),
            Answer::MeetsAFullDisk,
            Answer::Ends,
            Answer::Foreign(ForeignAct::Edit),
            Answer::Foreign(ForeignAct::Remove),
            Answer::Foreign(ForeignAct::Take),
        ] {
            assert_eq!(
                Answer::named(answer.name()).map(|it| it.name()),
                Some(answer.name())
            );
        }
    }
}
