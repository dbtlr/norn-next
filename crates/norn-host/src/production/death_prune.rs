//! The death prune's clock, its pass and its accounting, as the attachment
//! holds them ([ADR 0052]).
//!
//! The store runs a unit and holds the kept set; what is the host's is when a
//! pass is owed, the leg it runs in and the read-through generation each unit
//! is handed. A unit is the third unit kind of a maintenance leg, after the
//! shadow sweep and the rolling-heal slice, and like the sweep it does not
//! wait for a quiet entry.
//!
//! # The clock and its due cases
//!
//! The clock's period is the shadow sweep's, and it starts one period after
//! attach. When it comes due, a pass is owed only where new work could exist:
//!
//! 1. a death was recorded above the last pass's upper bound;
//! 2. a `link/broken` finding was filed or discarded since the last pass
//!    began; or
//! 3. the last pass kept a death for lane 2 alone, and the read-through
//!    generation has risen above the lowest one any of its removing units was
//!    handed.
//!
//! An attachment that has completed no pass owes one at its first period,
//! since what the cases read was lost with the last attachment. A pass in
//! progress keeps the death prune due. Where none holds, the clock moves a
//! period on and contributes no deadline until a case can hold again.
//!
//! The first two cases can come to hold only where lane-1 work commits, so the
//! attachment notes that a leg may have committed ([`DeathPrune::lane_one_may_have_moved`])
//! and reads the store's [`DeathPruneMarks`] only when the clock is due; the
//! third reads the engine's report. Neither reading does I/O under the entry
//! gate: the marks are read in the leg, holding the claim.
//!
//! [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md

use std::sync::Mutex;
use std::time::{Duration, Instant};

use norn_store::{
    DeathPruneMarks, DeathPrunePass, DeathPruneUnit, DeathPruneWork, ReadThrough, Store,
};

/// The death prune's clock period: the shadow sweep's.
pub const DEATH_PRUNE_PERIOD: Duration = norn_fs::SHADOW_AGE_THRESHOLD;

/// What the attachment holds for the death prune between legs: the clock, the
/// pass in progress and what the last completed pass left for the due cases.
///
/// A detach or a host restart loses it with the attachment, and a rebuild
/// resets it ([`DeathPrune::reset_after_rebuild`]): the next pass starts over,
/// owed at the clock's next period.
pub(super) struct DeathPrune {
    /// When the clock next comes due.
    pub(super) due: Instant,
    /// The clock's period: [`DEATH_PRUNE_PERIOD`] outside a case that
    /// shortens it.
    period: Duration,
    pass: DeathPrunePass,
    /// The store's marks as the pass in progress began: its upper bound, and
    /// the broken-link set it marked from.
    began: Option<DeathPruneMarks>,
    /// When the pass in progress was found owed, from which the clock's next
    /// period runs.
    began_at: Option<Instant>,
    /// What the last completed pass left for the due cases, `None` until one
    /// completes over this attachment.
    last: Option<LastPass>,
    /// Whether a leg may have committed lane-1 work since the clock last
    /// found nothing owed: the only way the first two due cases come to hold.
    moved: bool,
}

/// What a completed pass leaves for the due cases.
#[derive(Clone, Debug)]
struct LastPass {
    began: DeathPruneMarks,
    kept_for_lane_two_alone: bool,
    lowest_read_through: Option<ReadThrough>,
}

impl DeathPrune {
    /// A clock of `period` that comes due one period after `attached`, with
    /// no pass begun and the first pass owed.
    pub(super) fn new(attached: Instant, period: Duration) -> Self {
        DeathPrune {
            due: attached + period,
            period,
            pass: DeathPrunePass::new(),
            began: None,
            began_at: None,
            last: None,
            moved: false,
        }
    }

    /// Whether a pass has begun and not ended.
    pub(super) fn in_progress(&self) -> bool {
        self.began.is_some()
    }

    /// Note that a leg may have committed lane-1 work, which is how a death or
    /// a change to the broken-link set arrives: answers whether this is news,
    /// so the caller wakes the scheduler to read the clock again.
    pub(super) fn lane_one_may_have_moved(&mut self) -> bool {
        !std::mem::replace(&mut self.moved, true)
    }

    /// When the clock contributes a deadline, or `None` where no pass could be
    /// owed: the store's from-zero build has not ended or its link health is
    /// not admitted, or no due case can hold. `read_through` is consulted only
    /// for the third case.
    pub(super) fn deadline(
        &self,
        store: &Store,
        read_through: impl FnOnce() -> Option<ReadThrough>,
    ) -> Option<Instant> {
        if !admitted(store) {
            return None;
        }
        let could_be_owed = self.in_progress()
            || self.moved
            || match &self.last {
                None => true,
                Some(last) => lane_two_may_have_read(last, read_through()),
            };
        could_be_owed.then_some(self.due)
    }

    /// Whether the clock is due and, where no pass is in progress, a pass is
    /// owed over `marks` and `read_through`; where one is, begin it.
    ///
    /// A due clock that finds nothing owed moves a period on from `now`, and
    /// the leg runs no unit.
    pub(super) fn begin_if_owed(
        &mut self,
        now: Instant,
        marks: impl FnOnce() -> Result<DeathPruneMarks, norn_store::StoreError>,
        read_through: Option<ReadThrough>,
    ) -> Result<Owed, norn_store::StoreError> {
        if self.in_progress() {
            return Ok(Owed::Pass);
        }
        if self.due > now {
            return Ok(Owed::NotDue);
        }
        let marks = marks()?;
        self.moved = false;
        if !pass_owed(self.last.as_ref(), &marks, read_through) {
            self.due = now + self.period;
            return Ok(Owed::Nothing);
        }
        self.began = Some(marks);
        self.began_at = Some(now);
        self.pass = DeathPrunePass::new();
        Ok(Owed::Pass)
    }

    /// The pass a unit runs.
    pub(super) fn pass(&mut self) -> &mut DeathPrunePass {
        &mut self.pass
    }

    /// The upper bound of the pass in progress, as the store last fixed it.
    pub(super) fn upper_bound(&self) -> Option<i64> {
        self.pass.upper_bound()
    }

    /// Record the marks of a pass the store began again, where a unit started
    /// it over or fixed its bound: read after that unit, which took no
    /// generation, they describe the store as the pass began.
    pub(super) fn began_again(&mut self, marks: DeathPruneMarks) {
        self.began = Some(marks);
    }

    /// Take a unit's end: where it finished the pass, record what the due
    /// cases read next and move the clock a period on from when the pass was
    /// found owed.
    pub(super) fn unit_ended(&mut self, unit: &DeathPruneUnit) {
        if !unit.finished {
            return;
        }
        let began = self.began.take().expect("a finished pass had begun");
        self.last = Some(LastPass {
            began,
            kept_for_lane_two_alone: self.pass.kept_for_lane_two_alone(),
            lowest_read_through: self.pass.lowest_read_through(),
        });
        let began_at = self.began_at.take().unwrap_or_else(Instant::now);
        self.due = self.due.max(began_at + self.period);
        self.pass = DeathPrunePass::new();
    }

    /// Forget the pass and what the last one left, keeping the clock: the
    /// store was discarded and opened again, so its kept set and its marks are
    /// gone, and the next period owes a pass that starts over.
    pub(super) fn reset_after_rebuild(&mut self) {
        self.pass = DeathPrunePass::new();
        self.began = None;
        self.began_at = None;
        self.last = None;
    }
}

/// What a due check found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Owed {
    /// The clock has not come due.
    NotDue,
    /// The clock came due and no case holds: it moved a period on.
    Nothing,
    /// A pass is in progress, or was just begun.
    Pass,
}

/// Whether a death-prune unit may run over `store` now: its from-zero build
/// has ended and its link health is admitted. The store refuses a unit
/// otherwise; the host reads the same two facts first so a clock never comes
/// due over a store that would refuse.
pub(super) fn admitted(store: &Store) -> bool {
    store.construction_step().is_none() && !store.link_health_deferred()
}

/// Whether a pass is owed over `marks`, after `last`, with lane 2 having read
/// through `read_through`: the three due cases, or no pass completed yet.
fn pass_owed(
    last: Option<&LastPass>,
    marks: &DeathPruneMarks,
    read_through: Option<ReadThrough>,
) -> bool {
    let Some(last) = last else {
        return true;
    };
    let bound = last.began.write_generation;
    let death_recorded = marks.newest_death.is_some_and(|newest| newest > bound);
    let broken_moved = marks.newest_broken.is_some_and(|newest| newest > bound)
        || marks.broken_findings != last.began.broken_findings;
    death_recorded || broken_moved || lane_two_may_have_read(last, read_through)
}

/// The third due case: the last pass kept a death for lane 2 alone, and the
/// read-through generation now handed stands above the lowest any of its
/// removing units was handed. A vault whose engine has since been turned off
/// hands none, which keeps nothing for lane 2, so those deaths may go too.
fn lane_two_may_have_read(last: &LastPass, read_through: Option<ReadThrough>) -> bool {
    last.kept_for_lane_two_alone
        && match (last.lowest_read_through, read_through) {
            (Some(lowest), Some(now)) => now > lowest,
            (Some(_), None) => true,
            (None, _) => false,
        }
}

/// Whether a floor raised to `floor` passed the read-through generation the
/// unit was handed: it removed deaths a reader had not read, so the engine
/// owes a reconcile and is woken for it.
pub(super) fn passes_read_through(floor: i64, read_through: Option<ReadThrough>) -> bool {
    match read_through {
        None => false,
        Some(ReadThrough::Nothing) => true,
        Some(ReadThrough::Through(read)) => floor > read,
    }
}

/// What this host's death-prune units have done, counted since the host began,
/// so a counter case can bound a pass's work against what it examined.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeathPruneReading {
    /// Due clocks that read the store's marks.
    pub due_checks: u64,
    /// Passes begun.
    pub passes_begun: u64,
    /// Passes finished.
    pub passes_finished: u64,
    /// Units run.
    pub units: u64,
    /// Units that raised the death floor.
    pub floor_raises: u64,
    /// Floor raises that passed the read-through generation the unit was
    /// handed, and so woke the vault's engine.
    pub engine_wakes: u64,
    /// The units' work, summed. `live_documents` sums every count a unit took.
    pub work: DeathPruneWork,
}

/// The host's account of its death-prune units.
#[derive(Debug, Default)]
pub(crate) struct DeathPrunes {
    reading: Mutex<DeathPruneReading>,
}

impl DeathPrunes {
    pub(crate) fn reading(&self) -> DeathPruneReading {
        *self
            .reading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn count(&self, counted: impl FnOnce(&mut DeathPruneReading)) {
        counted(
            &mut self
                .reading
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

/// `total` with `unit`'s work added.
pub(super) fn add_work(total: &mut DeathPruneWork, unit: &DeathPruneWork) {
    total.rows_read += unit.rows_read;
    total.findings_marked += unit.findings_marked;
    total.catch_up_findings += unit.catch_up_findings;
    total.broken_links_marked += unit.broken_links_marked;
    total.followed_reads += unit.followed_reads;
    total.deaths_marked += unit.deaths_marked;
    if let Some(live) = unit.live_documents {
        total.live_documents = Some(total.live_documents.unwrap_or(0) + live);
    }
    total.deaths_walked += unit.deaths_walked;
    total.deaths_examined += unit.deaths_examined;
    total.deaths_removed += unit.deaths_removed;
    total.deaths_kept_by_link += unit.deaths_kept_by_link;
    total.deaths_kept_for_lane_two += unit.deaths_kept_for_lane_two;
    total.kept_rows_cleared += unit.kept_rows_cleared;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(
        write_generation: i64,
        newest_death: Option<i64>,
        broken: (Option<i64>, u64),
    ) -> DeathPruneMarks {
        DeathPruneMarks {
            write_generation,
            newest_death,
            newest_broken: broken.0,
            broken_findings: broken.1,
        }
    }

    fn last(kept_for_lane_two_alone: bool, lowest: Option<ReadThrough>) -> LastPass {
        LastPass {
            began: marks(10, Some(9), (Some(8), 2)),
            kept_for_lane_two_alone,
            lowest_read_through: lowest,
        }
    }

    /// **A pass is owed only where new work could exist.** No pass yet owes
    /// one; then a death above the last bound, a broken finding filed since,
    /// one discarded since, or lane 2 reading past what the last pass's
    /// removing units were handed, where it kept a death for lane 2 alone.
    #[test]
    fn a_pass_is_owed_only_on_its_due_cases() {
        let quiet = marks(14, Some(9), (Some(8), 2));
        assert!(pass_owed(None, &quiet, None), "the first pass is owed");
        let done = last(false, Some(ReadThrough::Through(5)));
        assert!(
            !pass_owed(Some(&done), &quiet, None),
            "writes alone owe nothing"
        );
        assert!(
            pass_owed(Some(&done), &marks(14, Some(12), (Some(8), 2)), None),
            "a death recorded above the last bound"
        );
        assert!(
            !pass_owed(Some(&done), &marks(14, Some(10), (Some(8), 2)), None),
            "a death at the bound was the last pass's"
        );
        assert!(
            pass_owed(Some(&done), &marks(14, Some(9), (Some(13), 3)), None),
            "a broken finding filed"
        );
        assert!(
            pass_owed(Some(&done), &marks(14, Some(9), (Some(8), 1)), None),
            "a broken finding discarded"
        );
        assert!(
            pass_owed(Some(&done), &marks(14, Some(9), (None, 0)), None),
            "every broken finding discarded"
        );
    }

    /// **The third case compares against the lowest generation the removing
    /// units were handed, and a stalled engine makes no pass due.**
    #[test]
    fn lane_two_owes_a_pass_only_where_it_read_on_past_what_was_kept() {
        let quiet = marks(14, Some(9), (Some(8), 2));
        let kept = last(true, Some(ReadThrough::Through(5)));
        for stalled in [
            Some(ReadThrough::Through(5)),
            Some(ReadThrough::Through(4)),
            Some(ReadThrough::Nothing),
        ] {
            assert!(!pass_owed(Some(&kept), &quiet, stalled), "{stalled:?}");
        }
        assert!(pass_owed(
            Some(&kept),
            &quiet,
            Some(ReadThrough::Through(6))
        ));
        assert!(
            pass_owed(Some(&kept), &quiet, None),
            "the engine was turned off"
        );
        let nothing_kept = last(false, Some(ReadThrough::Through(5)));
        assert!(!pass_owed(
            Some(&nothing_kept),
            &quiet,
            Some(ReadThrough::Through(9))
        ));
        let read_nothing = last(true, Some(ReadThrough::Nothing));
        assert!(pass_owed(
            Some(&read_nothing),
            &quiet,
            Some(ReadThrough::Through(1))
        ));
    }

    /// A floor raise wakes the engine only where it passed what the unit was
    /// handed; a vault no reader stands for has no engine to wake.
    #[test]
    fn a_raise_passes_only_a_read_through_below_it() {
        assert!(!passes_read_through(7, None));
        assert!(passes_read_through(7, Some(ReadThrough::Nothing)));
        assert!(passes_read_through(7, Some(ReadThrough::Through(6))));
        assert!(!passes_read_through(7, Some(ReadThrough::Through(7))));
        assert!(!passes_read_through(7, Some(ReadThrough::Through(9))));
    }
}
