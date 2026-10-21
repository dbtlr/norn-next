#!/usr/bin/env bash
# Run one libtest target, rejecting a successful command that ran no case.
# Used by exact native checks and the ignored-lane runner. The final summary
# is authoritative: an earlier look-alike printed by a case proves nothing.

set -uo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 2
fi

log=$(mktemp) || exit 1
trap 'rm -f "$log"' EXIT

# Stream output so a timeout retains evidence. Keep the command's verdict even
# when its output contains a passing summary, and reject a failed log writer.
"$@" 2>&1 | tee "$log"
statuses=("${PIPESTATUS[@]}")
if [ "${statuses[0]}" -ne 0 ]; then
  exit "${statuses[0]}"
fi
if [ "${statuses[1]}" -ne 0 ]; then
  exit "${statuses[1]}"
fi

summary=$(grep -E '^test result: ' "$log" | tail -n1)
if ! printf '%s\n' "$summary" | grep -qE '^test result: ok\. [1-9][0-9]* passed'; then
  echo "::error::$* ran no passing test case."
  exit 1
fi
