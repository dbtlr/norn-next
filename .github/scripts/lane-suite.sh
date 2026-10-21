#!/usr/bin/env bash
#
# Run one suite's ignored cases, and fail when none of them ran.
#
# `cargo test -- --ignored` exits 0 when nothing matched. A suite whose cases
# were renamed, re-laned or deleted therefore reports a green step having
# measured nothing, which is the failure a measurement lane is least likely to
# notice. Selecting cases by kind rather than by name removes only the
# name-typo spelling of that hazard; **asserting the pass count is what closes
# it**, and that is what this script exists to do.
#
# Which lane a case belongs to is its `#[ignore]` reason, and the reasons are
# checked by norn_testkit::lanes, which walks each crate's tests/ directory
# against a table that crate owns (its tests/lanes.rs). A stray `#[ignore]`
# therefore cannot be silently adopted by whichever lane runs its suite.
#
# A suite whose cases sit behind a feature compiles to zero tests without it, so
# the step that runs one names the feature in `LANE_FEATURES`. It arrives in the
# environment rather than in the argument list because the two tokens after this
# script are the pairing `norn_testkit::lanes` reads as the package and the
# target a step adopts, and a flag between them would be read as one of those.
#
# usage: lane-suite.sh <package> <test-target> [extra harness args...]
#        LANE_FEATURES=<features> lane-suite.sh ...

set -uo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <package> <test-target> [extra harness args...]" >&2
  exit 2
fi

package=$1
target=$2
shift 2

# Expanded through the `+` form below, so an empty array is no argument at all
# rather than the unbound-variable error `set -u` raises for one on the bash 3.2
# that macOS ships.
features=()
if [ -n "${LANE_FEATURES:-}" ]; then
  features=(--features "$LANE_FEATURES")
fi

exec "$(dirname "$0")/test-suite.sh" cargo test --locked -p "$package" --test "$target" ${features[@]+"${features[@]}"} -- --ignored "$@"
