#!/usr/bin/env bash
# Run one libtest target, rejecting a successful command that ran no case.
# Shared by the ignored-lane runner and explicit single-target invocations. The final summary
# is authoritative: an earlier look-alike printed by a case proves nothing.

set -uo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 2
fi

log=$(mktemp) || exit 1
trap 'rm -f "$log"' EXIT

# Stream output so a timeout retains evidence. The command owns its verdict;
# a downstream failure cannot overturn complete evidence of successful work.
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
if [ "$status" -ne 0 ]; then
  exit "$status"
fi

summary=$(grep -E '^test result: ' "$log" | tail -n1)
if ! printf '%s\n' "$summary" | grep -qE '^test result: ok\. [1-9][0-9]* passed'; then
  echo "::error::$* ran no passing test case."
  exit 1
fi
