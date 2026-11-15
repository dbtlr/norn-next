use norn_host::ProductionPolicy;
use std::time::Duration;
#[test]
fn plain_values_construct_the_store_policies() {
    let policy = ProductionPolicy::serving(7, Duration::from_secs(3), Some(20), Some(40)).unwrap();
    assert_eq!(
        policy.read_budget,
        norn_store::ReadBudgetPolicy::new(7, Duration::from_secs(3)).unwrap()
    );
    let expected = norn_store::CountRetentionPolicy {
        max_result_bytes: Some(20),
        total_bytes: Some(40),
        ..Default::default()
    };
    assert_eq!(
        policy.count_retention.max_result_bytes,
        expected.max_result_bytes
    );
    assert_eq!(policy.count_retention.total_bytes, expected.total_bytes);
    assert_eq!(policy.count_retention.lifetime, expected.lifetime);
    assert_eq!(policy.count_retention.max_results, expected.max_results);
}
#[test]
fn reader_capacity_obeys_the_store_minimum() {
    for n in [0, 1] {
        assert!(ProductionPolicy::serving(n, Duration::ZERO, None, None).is_err());
    }
    assert!(ProductionPolicy::serving(2, Duration::ZERO, None, None).is_ok());
}

#[test]
fn serving_does_not_add_a_store_capacity_ceiling() {
    let direct = norn_store::ReadBudgetPolicy::new(100, Duration::ZERO).unwrap();
    let policy = ProductionPolicy::serving(100, Duration::ZERO, None, None).unwrap();
    assert_eq!(policy.read_budget, direct);
    assert_eq!(policy.count_retention.max_result_bytes, None);
    assert_eq!(policy.count_retention.total_bytes, None);
}

#[test]
fn serving_refuses_zero_and_inconsistent_count_quotas() {
    for (max, total) in [(Some(0), None), (None, Some(0)), (Some(2), Some(1))] {
        assert!(ProductionPolicy::serving(2, Duration::ZERO, max, total).is_err());
    }
    for (max, total) in [
        (None, None),
        (Some(1), None),
        (None, Some(1)),
        (Some(2), Some(2)),
    ] {
        assert!(ProductionPolicy::serving(2, Duration::ZERO, max, total).is_ok());
    }
}

#[test]
fn serving_preserves_store_policy_refusal() {
    let expected = norn_store::ReadBudgetPolicy::new(1, Duration::ZERO).unwrap_err();
    let error = ProductionPolicy::serving(1, Duration::ZERO, None, None).unwrap_err();
    assert_eq!(error.to_string(), expected.to_string());
}
