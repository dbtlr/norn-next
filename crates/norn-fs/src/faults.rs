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
//! The same shape carries the other half of what a test cannot arrange: a
//! foreign writer landing inside a window one call wide. [`Window`] names those
//! windows, and a disturbance is handed the window it is standing in.
//!
//! **The seam is deliberately small.** It names *where* a write can be made to
//! fail or be disturbed, and never *what happens next* — the answer to that is
//! the code under test. Both halves are `pub(crate)` and behind `cfg(test)`
//! where they take a value, so nothing outside this crate injects through them
//! today; a lockdown layer that wants the disk-full and process-kill bars widens
//! this seam rather than growing a second one, and that widening is its decision
//! to take.

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
    /// Removing a shadow, or a destination a create could not finish. Injecting
    /// here stands in for a removal the filesystem refused — the one condition
    /// whose required behavior is to change nothing at all about the outcome.
    Cleanup,
}

/// A point in the protocol where a foreign actor's act can be made to land.
///
/// Each window is one call wide in a real run, which is why a test arranges it
/// rather than races for it: the defense would otherwise be asserted instead of
/// checked. The windows are the ones where something outside this process can
/// make a statement the protocol is about to make untrue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Window {
    /// A replacement's precondition is satisfied and nothing is staged yet.
    Composed,
    /// A create has claimed its name and has not filled it.
    Claimed,
    /// A removal's precondition is satisfied and the name has not been confirmed
    /// to still resolve to the handle that was read.
    Vacating,
    /// A move's source has been read and the name has not been confirmed to
    /// still resolve to the handle it was read through.
    SourceRead,
    /// A move's destination holds the document and its source has not been
    /// removed yet.
    BetweenLegs,
}

/// Which stages of a write fail, and how.
///
/// A list rather than one entry, because two of the claims are about what
/// happens when a *second* thing goes wrong: a swap that fails and then a
/// cleanup that cannot happen either is the condition under which a shadow
/// leaks, and it is unreachable if only one stage at a time can be made to fail.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Faults {
    injected: &'static [(Stage, io::ErrorKind)],
}

impl Faults {
    /// A write that fails only where the machine makes it fail. This is what
    /// every public entry point passes.
    pub(crate) const NONE: Faults = Faults { injected: &[] };

    /// A write that fails at each named stage.
    #[cfg(test)]
    pub(crate) const fn at(injected: &'static [(Stage, io::ErrorKind)]) -> Faults {
        Faults { injected }
    }

    /// The error `stage` is supposed to meet, if it is one of the injected ones.
    pub(crate) fn check(&self, stage: Stage) -> io::Result<()> {
        for (injected, kind) in self.injected {
            if *injected == stage {
                return Err(io::Error::new(
                    *kind,
                    format!("injected failure at the {stage:?} stage"),
                ));
            }
        }
        Ok(())
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
            Stage::Cleanup,
        ] {
            Faults::NONE.check(stage).expect("no injected failure");
        }
    }

    /// One stage fails and the others do not, so a bar reaches the stage it
    /// names rather than the first one the protocol happens to run.
    #[test]
    fn an_injected_fault_fires_at_one_stage_only() {
        let faults = Faults::at(&[(Stage::Swap, io::ErrorKind::PermissionDenied)]);
        let error = faults.check(Stage::Swap).expect_err("the injected stage");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Swap"), "{error}");
        for other in [
            Stage::Create,
            Stage::Write,
            Stage::Sync,
            Stage::ParentSync,
            Stage::Cleanup,
        ] {
            faults.check(other).expect("a stage nothing injected");
        }
    }
}
