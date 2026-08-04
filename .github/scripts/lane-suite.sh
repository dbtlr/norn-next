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
# usage: lane-suite.sh <package> <test-target> [extra harness args...]

set -uo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <package> <test-target> [extra harness args...]" >&2
  exit 2
fi

package=$1
target=$2
shift 2

# The run is streamed to the log as it happens, rather than captured into a
# variable and printed only on exit: a step a timeout kills mid-run leaves
# nothing behind if the output is held until the command returns, which is
# exactly the moment a timeout never lets it reach. `PIPESTATUS[0]` names
# cargo's own exit status explicitly, rather than leaning on `pipefail` to
# route it through `$?` correctly — `tee` itself essentially never fails, so
# reading its index directly is the plain way to say which command's status
# this is.
log=$(mktemp)
trap 'rm -f "$log"' EXIT

cargo test --locked -p "$package" --test "$target" -- --ignored "$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

if [ "$status" -ne 0 ]; then
  exit "$status"
fi

# Anchored at the start of the line, and read from the last such line rather
# than any match: cargo's own summary is exactly one line reading `test
# result: ok. N passed; ...`, always the final one it prints, and a look-alike
# a test prints to its own stdout under `--nocapture` is not that line just
# because it matches the same shape somewhere earlier in the log.
summary=$(grep -E '^test result: ' "$log" | tail -n1)
if ! printf '%s\n' "$summary" | grep -qE '^test result: ok\. [1-9][0-9]* passed'; then
  echo "::error::${package} --test ${target} ran no ignored case, so this step passed having measured nothing."
  exit 1
fi
