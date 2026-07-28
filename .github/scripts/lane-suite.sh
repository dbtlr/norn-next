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
# checked by crates/norn-fixtures/tests/lanes.rs, so a stray `#[ignore]` cannot
# be silently adopted by whichever lane runs its suite.
#
# usage: lane-suite.sh <package> <test-target> [extra harness args...]

set -uo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <package> <test-target> [extra harness args...]" >&2
  exit 2
fi

package=$1
target=$2
shift 2

# The exit status is captured rather than acted on in place: the output has to
# reach the log whether the run passed or failed, and a pipeline would hand
# back the wrong status to gate on.
status=0
output=$(cargo test --locked -p "$package" --test "$target" -- --ignored "$@" 2>&1) || status=$?
printf '%s\n' "$output"

if [ "$status" -ne 0 ]; then
  exit "$status"
fi

if ! printf '%s\n' "$output" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; then
  echo "::error::${package} --test ${target} ran no ignored case, so this step passed having measured nothing."
  exit 1
fi
