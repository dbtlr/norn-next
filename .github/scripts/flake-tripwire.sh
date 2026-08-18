#!/usr/bin/env bash
#
# Run a suite, and where it fails, match its output against the flake ledger.
#
# **The bar this exists for**: a second occurrence of a ledgered failure
# surfaces as a record, never as a quiet rerun. An entry in
# `.github/flake-ledger` exists because that failure already happened and was
# ruled on, so a match here is the second occurrence by construction — and what
# it produces is an annotation on the run and a block in the job summary,
# naming the entry, the class and the ruling.
#
# **It changes no verdict.** The suite's own exit status is this script's, a
# match neither reds a green run nor greens a red one, and nothing here retries
# anything: a mechanism that could hide a failure would be the thing it was
# written to prevent.
#
# A failure that matches nothing is recorded too, as the new failure it is.
# Silence would otherwise read the same as a run nobody scanned.
#
# usage: flake-tripwire.sh <command> [argument...]

set -uo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: $0 <command> [argument...]" >&2
  exit 2
fi

here=$(cd -- "$(dirname -- "$0")/../.." && pwd)
ledger="${here}/.github/flake-ledger"
# The template is explicit: `mktemp -t <name>` without X's is a BSD spelling
# that GNU coreutils refuses, and a script that took an empty path here would
# scan nothing and say so.
log=$(mktemp "${TMPDIR:-/tmp}/norn-flake-tripwire.XXXXXX")
if [ -z "$log" ]; then
  echo "::warning::the flake tripwire could not make a scratch log, so this run was matched against nothing."
  exec "$@"
fi
trap 'rm -f "$log"' EXIT

"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

if [ "$status" -eq 0 ]; then
  exit 0
fi

if [ ! -r "$ledger" ]; then
  echo "::warning::the flake ledger is not readable at ${ledger}, so this failure was matched against nothing."
  exit "$status"
fi

run="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-a checkout}/actions/runs/${GITHUB_RUN_ID:-local}"
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

# One record per matched entry: the ruling as an annotation, and the whole
# entry as a block in the job summary. The awk program owns the parsing so the
# block format is read in one place; it prints one `field<TAB>value` stream per
# match, and the shell renders it.
matches=$(awk -v output="$log" '
  function flush() {
    if (id != "" && signature != "") {
      matched = ""
      while ((getline line < output) > 0) {
        if (index(line, signature) > 0) { matched = line; break }
      }
      close(output)
      if (matched != "") {
        printf "%s\t%s\t%s\t%s\t%s\t%s\n", id, signature, class, seen, disposition, matched
      }
    }
    id = ""; signature = ""; class = ""; seen = ""; disposition = ""
  }
  /^#/ { next }
  /^[[:space:]]*$/ { flush(); next }
  /^id: / { id = substr($0, 5); next }
  /^signature: / { signature = substr($0, 12); next }
  /^class: / { class = substr($0, 8); next }
  /^first-seen: / { seen = substr($0, 13); next }
  /^disposition: / { disposition = substr($0, 14); next }
  END { flush() }
' "$ledger")

if [ -z "$matches" ]; then
  {
    echo
    echo "### Flake tripwire: no ledgered signature matched"
    echo
    echo "This failure is not one \`.github/flake-ledger\` has an entry for. It is a new"
    echo "failure until somebody says otherwise."
    echo
    echo "- run: ${run}"
  } >> "$summary"
  echo "::notice title=Flake tripwire::this failure matched no ledgered signature, so it is a new one."
  exit "$status"
fi

while IFS=$'\t' read -r id signature class seen disposition matched; do
  [ -n "$id" ] || continue
  echo "::error title=Ledgered flake recurred: ${id}::${disposition}"
  {
    echo
    echo "### Flake tripwire: \`${id}\` recurred"
    echo
    echo "- **class**: ${class}"
    echo "- **first seen**: ${seen}"
    echo "- **signature**: \`${signature}\`"
    echo "- **disposition**: ${disposition}"
    echo "- **matched line**: \`${matched}\`"
    echo "- **run**: ${run}"
    echo
    echo "This is a second occurrence of a failure already ruled on. Read the disposition"
    echo "before rerunning: the ledger entry says what a recurrence means."
  } >> "$summary"
done <<< "$matches"

exit "$status"
