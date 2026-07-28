//! The size-independence pair: one operation, two vault scales, compared as
//! counts.
//!
//! A request that touches ten documents costs the same whether the vault
//! holds one thousand or one hundred thousand. The way that claim is checked
//! is a pair — the same operation run against two generated profiles of
//! different scale, with the counters compared name by name. **Counts, never
//! clocks**: a clock says as much about the machine as about the program, and
//! this comparison belongs to the per-PR lane.
//!
//! The scales are [`Profile`]s rather than document counts written by hand,
//! so a pair names two trees that can actually be generated and takes their
//! sizes from the generator rather than from a comment.

use norn_fixtures::Profile;

use crate::counters::CounterSnapshot;

/// One run of the operation, against one scale.
pub struct ScaleObservation<'a> {
    pub profile: &'a Profile,
    pub counters: CounterSnapshot,
}

impl<'a> ScaleObservation<'a> {
    pub fn new(profile: &'a Profile, counters: CounterSnapshot) -> Self {
        ScaleObservation { profile, counters }
    }
}

/// Two runs of one operation, at two scales.
pub struct SizeIndependencePair<'a> {
    pub operation: &'a str,
    pub small: ScaleObservation<'a>,
    pub large: ScaleObservation<'a>,
}

impl<'a> SizeIndependencePair<'a> {
    pub fn new(
        operation: &'a str,
        small: ScaleObservation<'a>,
        large: ScaleObservation<'a>,
    ) -> Self {
        SizeIndependencePair {
            operation,
            small,
            large,
        }
    }

    /// Every way the pair fails to demonstrate size independence, one line
    /// each. A sound pair produces none.
    ///
    /// The scale check is part of the assertion, not a precondition someone
    /// remembered: two runs against equally large vaults agree about
    /// everything and prove nothing about size.
    pub fn violations(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let (small, large) = (self.small.profile, self.large.profile);
        if large.docs <= small.docs {
            problems.push(format!(
                "`{}` is paired against `{}` at {} documents and `{}` at {}, so the pair spans no \
                 difference in scale",
                self.operation, small.name, small.docs, large.name, large.docs
            ));
        }
        if self.small.counters.is_empty() && self.large.counters.is_empty() {
            problems.push(format!(
                "`{}` recorded no counters at either scale, so the pair compares nothing",
                self.operation
            ));
        }
        for difference in self.small.counters.differences(&self.large.counters) {
            problems.push(format!(
                "`{}` counted differently across scales: {difference}",
                self.operation
            ));
        }
        problems
    }

    /// **The size-independence bar.** The operation counts the same at both
    /// scales.
    pub fn assert_size_independent(&self) {
        let problems = self.violations();
        assert!(
            problems.is_empty(),
            "size independence fails: {}",
            problems.join("; ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> Profile {
        Profile::by_name(name).expect("a named profile")
    }

    fn counters(values: &[(&str, u64)]) -> CounterSnapshot {
        values.iter().copied().collect()
    }

    #[test]
    fn an_operation_counting_the_same_at_both_scales_passes() {
        let (small, large) = (profile("tiny"), profile("realistic"));
        SizeIndependencePair::new(
            "suffix resolve",
            ScaleObservation::new(&small, counters(&[("rows_read", 4)])),
            ScaleObservation::new(&large, counters(&[("rows_read", 4)])),
        )
        .assert_size_independent();
    }

    #[test]
    #[should_panic(expected = "counted differently across scales")]
    fn an_operation_whose_counts_grow_with_the_vault_fails() {
        let (small, large) = (profile("tiny"), profile("realistic"));
        SizeIndependencePair::new(
            "suffix resolve",
            ScaleObservation::new(&small, counters(&[("rows_read", 4)])),
            ScaleObservation::new(&large, counters(&[("rows_read", 40)])),
        )
        .assert_size_independent();
    }

    #[test]
    #[should_panic(expected = "no difference in scale")]
    fn a_pair_of_equally_large_vaults_proves_nothing() {
        let (small, large) = (profile("realistic"), profile("realistic"));
        SizeIndependencePair::new(
            "suffix resolve",
            ScaleObservation::new(&small, counters(&[("rows_read", 4)])),
            ScaleObservation::new(&large, counters(&[("rows_read", 4)])),
        )
        .assert_size_independent();
    }

    #[test]
    #[should_panic(expected = "recorded no counters")]
    fn a_pair_comparing_nothing_fails() {
        let (small, large) = (profile("tiny"), profile("realistic"));
        SizeIndependencePair::new(
            "suffix resolve",
            ScaleObservation::new(&small, CounterSnapshot::new()),
            ScaleObservation::new(&large, CounterSnapshot::new()),
        )
        .assert_size_independent();
    }
}
