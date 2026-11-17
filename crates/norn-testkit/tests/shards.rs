//! The package-shard certification, run against this workspace: every package
//! is tested in exactly one PR job with the selection `ci.yml` gives it, and
//! each package shard builds every package's tests with the features a
//! whole-workspace `cargo test` would. The logic is
//! unit-tested in `norn_testkit::shards`.

use norn_testkit::shards::{Shard, TreeReading, shards_in, violations};

#[test]
fn each_package_is_tested_in_one_pr_job_and_each_shard_builds_with_the_whole_workspace_feature_set()
{
    let workflow = include_str!("../../../.github/workflows/ci.yml");
    let whole = TreeReading::read(&["--workspace".to_string()]).expect("reading the workspace");
    let shards: Vec<(Shard, TreeReading)> = shards_in(workflow)
        .expect("reading the shards from ci.yml")
        .into_iter()
        .map(|shard| {
            let reading = TreeReading::read(&shard.selection).expect("reading a shard");
            (shard, reading)
        })
        .collect();
    assert_eq!(violations(&whole, &shards), Vec::<String>::new());
}
