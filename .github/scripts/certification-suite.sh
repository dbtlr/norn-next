#!/usr/bin/env bash
#
# Run one certification suite and keep its harness output as the lane's log.
#
# The lane's qualification record carries an outcome per required case, and the
# outcomes are read off these logs by
# `norn_testkit::certification::lane::outcomes_from_logs`. What that module
# reads is the harness's own `test <name> ... <outcome>` lines, so this script
# keeps the whole run rather than a verdict of its own: a wrapper that mapped a
# suite's exit status onto its cases would record every case of a failing suite
# as failed, including the ones that passed before it.
#
# The log's name is the contract with that reader: `<package>__<target>.log` in
# the directory `NORN_CERTIFICATION_LOGS` names. A bare test-function name is
# ambiguous across targets, and the inventory's carrier is a file and a
# function — so the pair travels in the name.
#
# A target is an integration suite's file stem under `tests/`, or `lib` for the
# package's library: some cases are carried by unit tests, and a lane that ran
# only the integration suites would record those as never attempted.
#
# A suite whose cases sit behind a feature compiles to zero tests without it and
# leaves a log saying nothing ran, so the feature is this script's third
# argument and belongs to the target rather than to the lane.
#
# usage: certification-suite.sh <package> <lib|target stem> [feature]

set -uo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <package> <target> [feature]" >&2
  exit 2
fi

package=$1
target=$2
feature=${3:-}

if [ -z "${NORN_CERTIFICATION_LOGS:-}" ]; then
  echo "::error::NORN_CERTIFICATION_LOGS names the directory this lane's logs go in, and is unset." >&2
  exit 2
fi

mkdir -p "$NORN_CERTIFICATION_LOGS"
log="${NORN_CERTIFICATION_LOGS}/${package}__${target}.log"

# Expanded through the `+` form below, so an empty array is no argument at all
# rather than the unbound-variable error `set -u` raises for one on the bash
# 3.2 that macOS ships.
features=()
if [ -n "$feature" ]; then
  features=(--features "$feature")
fi

if [ "$target" = lib ]; then
  selector=(--lib)
else
  selector=(--test "$target")
fi

# Streamed to the job log as it happens and captured at the same time: a step a
# timeout kills mid-run leaves nothing behind if the output is held until the
# command returns. `PIPESTATUS[0]` names cargo's own status rather than tee's.
cargo test --locked -p "$package" "${selector[@]}" ${features[@]+"${features[@]}"} 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

if [ "$status" -ne 0 ]; then
  exit "$status"
fi

# A green run that executed nothing is the failure this lane is least likely to
# notice: the record would carry `not-run` for every case of the target and
# classify as a suite change, which reads as an inventory edit rather than as a
# lane that lost its cases. The summary line is the harness's last `test
# result:` line, and a look-alike in a suite's own output is not it.
summary=$(grep -E '^test result: ' "$log" | tail -n1)
if ! printf '%s\n' "$summary" | grep -qE '^test result: ok\. [1-9][0-9]* passed'; then
  echo "::error::${package} ${selector[*]} ran no certification case, so this step passed having certified nothing."
  exit 1
fi
