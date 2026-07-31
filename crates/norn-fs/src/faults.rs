//! Making a write fail without the environment cooperating.
//!
//! Two of this kernel's contract claims are about conditions a test cannot
//! arrange: a disk that fills between the shadow's first byte and its last, and
//! an fsync that fails after a rename has already published a name. Both are
//! real, both are the reason the code has the shape it has, and neither is
//! reachable by writing files into a temporary directory.
//!
//! So each stage of the protocol asks [`Faults`] whether it is the stage that
//! fails. The public entry points pass [`Faults::NONE`], which asks nothing and
//! costs one comparison; the in-crate suite passes a stage and an error and
//! reads what the protocol did with it.
//!
//! **The seam is deliberately small.** It names *where* a write can be made to
//! fail and never *what happens next* — the answer to that is the code under
//! test. The formal disk-full and process-kill bars are the lockdown layer's,
//! and they inject through this same seam rather than growing a second one.

use std::io;

/// A point in the write protocol that can be made to fail.
///
/// The stages are the ones whose failure has a *different* required outcome,
/// which is what makes each of them worth naming. A failure before the swap
/// refuses and leaves the destination alone; a failure after it has already
/// published a name and must never read as a write that did not happen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Opening the shadow, or the destination of an exclusive create.
    Create,
    /// Putting the content into it.
    Write,
    /// Getting those bytes onto the disk, before any name points at them.
    Sync,
    /// The rename that publishes them.
    Swap,
    /// The parent directory's fsync, which happens after the name is already
    /// live.
    ParentSync,
}

/// Which stage of a write fails, and how.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Faults {
    fail_at: Option<(Stage, io::ErrorKind)>,
}

impl Faults {
    /// A write that fails only where the machine makes it fail. This is what
    /// every public entry point passes.
    pub(crate) const NONE: Faults = Faults { fail_at: None };

    /// A write that fails at `stage` with `kind`.
    #[cfg(test)]
    pub(crate) const fn at(stage: Stage, kind: io::ErrorKind) -> Faults {
        Faults {
            fail_at: Some((stage, kind)),
        }
    }

    /// The error `stage` is supposed to meet, if it is the injected one.
    pub(crate) fn check(&self, stage: Stage) -> io::Result<()> {
        match self.fail_at {
            Some((injected, kind)) if injected == stage => Err(io::Error::new(
                kind,
                format!("injected failure at the {stage:?} stage"),
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam asks nothing when nothing is injected. A default that failed
    /// somewhere would make every ordinary write a test of this module.
    #[test]
    fn no_fault_lets_every_stage_through() {
        for stage in [
            Stage::Create,
            Stage::Write,
            Stage::Sync,
            Stage::Swap,
            Stage::ParentSync,
        ] {
            Faults::NONE.check(stage).expect("no injected failure");
        }
    }

    /// One stage fails and the others do not, so a bar reaches the stage it
    /// names rather than the first one the protocol happens to run.
    #[test]
    fn an_injected_fault_fires_at_one_stage_only() {
        let faults = Faults::at(Stage::Swap, io::ErrorKind::PermissionDenied);
        let error = faults.check(Stage::Swap).expect_err("the injected stage");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Swap"), "{error}");
        for other in [Stage::Create, Stage::Write, Stage::Sync, Stage::ParentSync] {
            faults.check(other).expect("a stage nothing injected");
        }
    }
}
