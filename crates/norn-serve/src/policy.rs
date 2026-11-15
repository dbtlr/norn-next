//! The two policies a host is composed from, built from the serving settings.
//!
//! `LifecyclePolicy` has no library default by design, so the serving entry
//! names every field: the three knobs the settings carry, and the production
//! constants `norn-host` names for the rest. `ProductionPolicy` holds store
//! types this crate has no edge to, so `norn-host` validates the plain values
//! into it.

use norn_config::machine::settings::ServingSettings;
use norn_host::{
    LifecyclePolicy, ProductionPolicy, ProductionPolicyError, READ_SETTLE_BOUND,
    WATCH_POLL_INTERVAL, WORKER_SLOTS,
};

pub(crate) fn lifecycle(settings: &ServingSettings) -> LifecyclePolicy {
    LifecyclePolicy {
        idle_after: settings.idle_after,
        worker_slots: WORKER_SLOTS,
        background_threads: settings.background_threads,
        watch_poll_interval: WATCH_POLL_INTERVAL,
        read_settle_bound: READ_SETTLE_BOUND,
        background_turn_budget: settings.background_turn_budget,
    }
}

pub(crate) fn production(
    settings: &ServingSettings,
) -> Result<ProductionPolicy, ProductionPolicyError> {
    ProductionPolicy::serving(
        settings.read_connections,
        settings.read_idle_grace,
        settings.count_result_max_bytes,
        settings.count_results_total_bytes,
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_lifecycle_takes_its_knobs_from_the_settings_and_the_rest_from_production() {
        let settings = ServingSettings {
            idle_after: Duration::from_secs(120),
            background_threads: 3,
            background_turn_budget: Duration::from_millis(40),
            ..ServingSettings::default()
        };
        let policy = lifecycle(&settings);
        assert_eq!(policy.idle_after, Duration::from_secs(120));
        assert_eq!(policy.background_threads, 3);
        assert_eq!(policy.background_turn_budget, Duration::from_millis(40));
        assert_eq!(policy.worker_slots, WORKER_SLOTS);
        assert_eq!(policy.watch_poll_interval, WATCH_POLL_INTERVAL);
        assert_eq!(policy.read_settle_bound, READ_SETTLE_BOUND);
    }

    #[test]
    fn every_default_setting_composes_a_production_policy() {
        assert!(production(&ServingSettings::default()).is_ok());
    }
}
