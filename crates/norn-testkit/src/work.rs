//! The work bar: how much engine work a drain of `n` rows is allowed to cost.
//!
//! A plan bar reads the shape SQLite *reported* for a statement, and a statement
//! bar reads the text the crate emitted. Both are judgments about wording. This
//! one is a judgment about work: it takes the step count the engine actually
//! spent — [`norn_store::Request::read_steps`] — and holds it under a line in
//! the number of rows drained.
//!
//! **Why a line.** A paged reader that seeks its cursor pays a bounded amount
//! per page, so a drain of `n` rows costs `floor + coefficient × n`. A reader
//! whose cursor has been demoted to a filter — the shape a novel spelling of the
//! same predicate can reintroduce without changing the reported plan's index
//! name — re-reads the rows ahead of its own position and costs a multiple of
//! `n²`. Any line in `n` separates the two once `n` is large enough, which is
//! what makes this bar hold a spelling nobody anticipated.
//!
//! **Why the numbers are not specified.** A step count is engine-version
//! sensitive: the same statement over the same rows steps a different number of
//! times under a different SQLite build. So a bar states a coefficient measured
//! against the build in the lockfile plus an absorber wide enough to survive an
//! engine bump, and its authority comes from the negative control beside it
//! rather than from the number's tightness. A bar with no failing control is a
//! bar that says nothing.

/// A line in the row count: what a drain of `n` rows may cost.
///
/// `floor` absorbs what a drain pays once — preparing, opening the cursor, the
/// final page that comes back empty. `per_row` is the measured coefficient plus
/// its absorber.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkBar {
    pub floor: u64,
    pub per_row: u64,
}

impl WorkBar {
    /// The most a drain of `rows` rows may step.
    pub fn ceiling(&self, rows: u64) -> u64 {
        self.floor.saturating_add(self.per_row.saturating_mul(rows))
    }

    /// **The bar.** `steps` is under the line this bar draws through `rows`.
    ///
    /// The failure prints the reading, the ceiling and the per-row cost that
    /// was actually paid, because what a reader wants next is whether the
    /// coefficient drifted or the shape changed.
    pub fn assert_within(&self, subject: &str, rows: u64, steps: u64) {
        let ceiling = self.ceiling(rows);
        assert!(
            steps <= ceiling,
            "{subject}: draining {rows} rows stepped the engine {steps} times, over the {ceiling} \
             this bar allows ({} + {} per row). Paid {} steps per row. A reading a little over \
             the line is a coefficient to re-measure; a reading that grows with the square of the \
             row count is a cursor that stopped seeking and started filtering.",
            self.floor,
            self.per_row,
            crate::readings::multiple(crate::readings::per_mille(steps, rows)),
        );
    }

    /// **The negative control.** `steps` is *over* the line, which is what says
    /// the bar can fail.
    ///
    /// A work bar's whole claim is that it separates a reader that seeks from
    /// one that re-reads. Asserting that the quadratic shape really does exceed
    /// the ceiling is what makes the passing reading beside it mean something:
    /// without it, a bar wide enough to admit anything passes exactly the same
    /// way.
    pub fn assert_exceeded(&self, subject: &str, rows: u64, steps: u64) {
        let ceiling = self.ceiling(rows);
        assert!(
            steps > ceiling,
            "{subject}: the control drained {rows} rows in {steps} steps, which is inside the \
             {ceiling} this bar allows. The control exists to prove the bar can fail, so a \
             control that passes it means the bar admits the shape it was drawn to exclude — \
             widen the control's row count or narrow the bar."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::WorkBar;

    const BAR: WorkBar = WorkBar {
        floor: 100,
        per_row: 10,
    };

    #[test]
    fn a_ceiling_is_the_floor_plus_the_coefficient_over_the_rows() {
        assert_eq!(BAR.ceiling(0), 100);
        assert_eq!(BAR.ceiling(50), 600);
    }

    #[test]
    fn a_reading_exactly_at_the_ceiling_is_admitted() {
        BAR.assert_within("at the line", 50, 600);
    }

    #[test]
    #[should_panic(expected = "over the 600 this bar allows")]
    fn a_reading_one_step_past_the_ceiling_is_refused() {
        BAR.assert_within("one past the line", 50, 601);
    }

    #[test]
    #[should_panic(expected = "The control exists to prove the bar can fail")]
    fn a_control_that_stays_under_the_ceiling_is_refused() {
        BAR.assert_exceeded("a control that proves nothing", 50, 600);
    }

    /// The line is what separates the two shapes, and it does so at a row count
    /// a suite can afford: at 200 rows a quadratic drain costs an order of
    /// magnitude more than this bar admits.
    #[test]
    fn a_quadratic_drain_is_outside_the_line_a_linear_one_sits_under() {
        let rows = 200u64;
        let linear = BAR.floor + 8 * rows;
        let quadratic = 8 * rows * rows / 2;
        BAR.assert_within("a reader that seeks", rows, linear);
        BAR.assert_exceeded("a reader that re-reads", rows, quadratic);
    }
}
