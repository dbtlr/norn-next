//! Counter snapshots, and the comparisons made over them.
//!
//! Counters are the per-PR lane's currency: they say how much work a request
//! did, and unlike a clock they say the same thing on a loaded machine. A
//! snapshot is an ordered map from counter name to value, so two snapshots
//! compare name by name and a counter that appears in one and not the other
//! is a difference rather than a silent skip.
//!
//! Nothing here reads a counter from anywhere. A snapshot is handed in by
//! whatever owns the counters, which keeps the comparison primitives usable
//! before their source exists and keeps this crate out of the substrate.

use std::collections::{BTreeMap, BTreeSet};

/// One reading of a set of counters.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CounterSnapshot {
    values: BTreeMap<String, u64>,
}

impl CounterSnapshot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name: impl Into<String>, value: u64) {
        self.values.insert(name.into(), value);
    }

    /// The counter's value, or zero where the snapshot does not carry it.
    pub fn get(&self, name: &str) -> u64 {
        self.values.get(name).copied().unwrap_or(0)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The counters that moved, in name order.
    pub fn nonzero(&self) -> Vec<(&str, u64)> {
        self.values
            .iter()
            .filter(|(_, value)| **value != 0)
            .map(|(name, value)| (name.as_str(), *value))
            .collect()
    }

    /// **The zero-on-warm assertion.** A warm request derives nothing, so
    /// every derivation counter it moves is a defect.
    pub fn assert_all_zero(&self, what: &str) {
        let moved = self.nonzero();
        assert!(
            moved.is_empty(),
            "{what} moved counters that a warm path leaves at zero: {moved:?}"
        );
    }

    /// The change from this snapshot to `later`, counter by counter.
    ///
    /// Both snapshots carry the same counter names or this is an error: a
    /// counter that appears between two readings means the two readings are
    /// of different things, and subtracting across that is arithmetic on a
    /// mistake.
    pub fn delta(&self, later: &CounterSnapshot) -> Result<CounterSnapshot, String> {
        let mine: BTreeSet<&str> = self.names().collect();
        let theirs: BTreeSet<&str> = later.names().collect();
        if mine != theirs {
            return Err(format!(
                "two readings carry different counters: {:?} against {:?}",
                mine, theirs
            ));
        }
        let mut delta = CounterSnapshot::new();
        for name in mine {
            let before = self.get(name);
            let after = later.get(name);
            if after < before {
                return Err(format!(
                    "counter `{name}` fell from {before} to {after}; a counter does not run \
                     backwards within one run"
                ));
            }
            delta.set(name, after - before);
        }
        Ok(delta)
    }

    /// Where two snapshots disagree, one line each.
    pub fn differences(&self, other: &CounterSnapshot) -> Vec<String> {
        let mut problems = Vec::new();
        let names: BTreeSet<&str> = self.names().chain(other.names()).collect();
        for name in names {
            let (mine, theirs) = (self.get(name), other.get(name));
            if mine != theirs {
                problems.push(format!("`{name}`: {mine} against {theirs}"));
            }
        }
        problems
    }

    /// **The size-independence comparison primitive.** Two runs of the same
    /// operation move the same counters, whatever the vault around them
    /// weighs.
    pub fn assert_equal_counts(&self, other: &CounterSnapshot, what: &str) {
        let differences = self.differences(other);
        assert!(
            differences.is_empty(),
            "{what} counted differently across two runs: {differences:?}"
        );
    }
}

impl FromIterator<(String, u64)> for CounterSnapshot {
    fn from_iter<I: IntoIterator<Item = (String, u64)>>(iter: I) -> Self {
        CounterSnapshot {
            values: iter.into_iter().collect(),
        }
    }
}

impl<'a> FromIterator<(&'a str, u64)> for CounterSnapshot {
    fn from_iter<I: IntoIterator<Item = (&'a str, u64)>>(iter: I) -> Self {
        iter.into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(values: &[(&str, u64)]) -> CounterSnapshot {
        values.iter().copied().collect()
    }

    #[test]
    fn a_snapshot_reports_the_counters_that_moved() {
        let snapshot = snapshot(&[("documents_parsed", 3), ("rows_upserted", 0)]);
        assert_eq!(snapshot.nonzero(), vec![("documents_parsed", 3)]);
        assert_eq!(snapshot.get("documents_parsed"), 3);
        assert_eq!(snapshot.get("never_recorded"), 0);
    }

    #[test]
    fn an_all_zero_snapshot_passes_the_warm_assertion() {
        snapshot(&[("documents_parsed", 0)]).assert_all_zero("a warm read");
    }

    #[test]
    #[should_panic(expected = "moved counters that a warm path leaves at zero")]
    fn a_counter_that_moved_fails_the_warm_assertion() {
        snapshot(&[("documents_parsed", 1)]).assert_all_zero("a warm read");
    }

    #[test]
    fn a_delta_subtracts_counter_by_counter() {
        let before = snapshot(&[("a", 1), ("b", 10)]);
        let after = snapshot(&[("a", 4), ("b", 10)]);
        assert_eq!(
            before.delta(&after).unwrap(),
            snapshot(&[("a", 3), ("b", 0)])
        );
    }

    #[test]
    fn a_delta_across_different_counter_sets_is_refused() {
        let before = snapshot(&[("a", 1)]);
        let after = snapshot(&[("a", 1), ("b", 1)]);
        let error = before
            .delta(&after)
            .expect_err("two readings of different things");
        assert!(error.contains("different counters"), "{error}");
    }

    #[test]
    fn a_counter_running_backwards_is_refused() {
        let error = snapshot(&[("a", 4)])
            .delta(&snapshot(&[("a", 1)]))
            .expect_err("a counter that fell");
        assert!(error.contains("does not run backwards"), "{error}");
    }

    #[test]
    fn equal_counts_compare_name_by_name() {
        let one = snapshot(&[("a", 2), ("b", 0)]);
        one.assert_equal_counts(&snapshot(&[("a", 2)]), "a read");
        assert_eq!(
            one.differences(&snapshot(&[("a", 3)])),
            vec!["`a`: 2 against 3".to_string()]
        );
    }

    #[test]
    #[should_panic(expected = "counted differently across two runs")]
    fn a_counter_present_in_one_run_only_is_a_difference() {
        snapshot(&[("a", 1)]).assert_equal_counts(&snapshot(&[("a", 1), ("b", 1)]), "a read");
    }
}
