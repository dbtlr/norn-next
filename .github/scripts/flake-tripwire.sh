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
# **Which bound a failed wait breached is named apart**, whether or not a
# ledger entry matched, because the two are different claims with different
# dispositions. A probe-bound breach is one evaluation that took longer than
# its bound: a reading of the runner, which was not scheduled for that long. A
# work-bound breach is a condition that never held, with no probe-bound breach
# rendered behind it: that wait carries no evidence of a starved probe, so under
# a class-A entry it reopens that ruling. A run that breached both is recorded
# under both and takes neither reading of the run.
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
#
# **A signature is matched against the lines a failure can be in, and no
# others.** libtest prints one status line per test it ran whatever the outcome,
# so a signature naming a test is in the output of a run where that test passed
# exactly as it is in a run where it failed — and a whole-log scan would report
# every ledgered class as recurring the first time anything else in the same
# suite went red. A line reporting a pass or a skip is therefore not scanned.
# Everything else is: the `... FAILED` status lines, the `failures:` list at the
# end, the panic text a signature naming an assertion lives in, and whatever
# else the run printed.
matches=$(awk -v output="$log" '
  function reports_a_test_that_did_not_fail(line) {
    return line ~ /^[ \t]*test .+ [.][.][.] (ok|ignored)/
  }
  function flush() {
    if (id != "" && signature != "") {
      matched = ""
      while ((getline line < output) > 0) {
        if (reports_a_test_that_did_not_fail(line)) continue
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

# The phrases `norn-testkit`'s wait module renders for each bound, and for no
# other: a probe-bound breach says `waiting for <what> stopped at probe <n>,
# which took <d> and passed its <d> probe bound <d> into a <d> work bound`, and
# a work-bound breach says `waiting for <what> passed its <d> work bound after
# <d> and <n> probes`. A bare `probe bound` is not the phrase, because the wait
# module's own assertions say it about waits that breached nothing. The probe
# phrase is also the signature of the ledger's class-a-probe-bound entry.
probe_bound_phrase="stopped at probe"
work_bound_phrase="work bound after"

# The lines of the run that carry `phrase`, read the way a signature is: a line
# reporting a test that passed or was skipped is not scanned.
lines_carrying() {
  awk -v phrase="$1" '
    /^[ \t]*test .+ [.][.][.] (ok|ignored)/ { next }
    index($0, phrase) > 0 { print }
  ' "$log"
}

# One job-summary block per bound a failed wait breached, naming what that
# breach is a reading of and the lines that say so. A run that breached both
# gets a third block and no verdict on the run from either of the first two:
# the ruling reads a run that starved a probe as non-qualifying and a work-bound
# breach with no probe-bound breach behind it as reopening a class-A ruling, and
# does not say whether a probe starved in one wait stands behind a work bound
# passed in another.
name_the_breached_bounds() {
  local starved elapsed line
  starved=$(lines_carrying "$probe_bound_phrase")
  elapsed=$(lines_carrying "$work_bound_phrase")
  if [ -n "$starved" ]; then
    {
      echo
      echo "### Flake tripwire: a probe bound was breached"
      echo
      echo "One evaluation of a wait's condition took longer than its probe bound. A probe"
      echo "takes a reading and returns, so this is a reading of the runner rather than the"
      echo "product: the process was not scheduled for that long. A probe that breaches its"
      echo "bound run after run is the structural cost the bound exists to catch, and these"
      echo "records are what show it."
      if [ -z "$elapsed" ]; then
        echo
        echo "This run is a non-qualifying evidence source on its own timing, and a rerun of"
        echo "it is deliberate and recorded."
      fi
      echo
      while IFS= read -r line; do
        echo "- **line**: \`${line}\`"
      done <<< "$starved"
    } >> "$summary"
  fi
  if [ -n "$elapsed" ]; then
    {
      echo
      echo "### Flake tripwire: a work bound was breached"
      echo
      echo "A wait's condition never held inside its work bound, and that wait rendered no"
      echo "probe-bound breach. Under a class-A ledger entry a recurrence like this"
      echo "reopens that ruling."
      echo
      while IFS= read -r line; do
        echo "- **line**: \`${line}\`"
      done <<< "$elapsed"
    } >> "$summary"
  fi
  if [ -n "$starved" ] && [ -n "$elapsed" ]; then
    {
      echo
      echo "### Flake tripwire: a probe bound and a work bound were both breached"
      echo
      echo "The ruling reads a run that starved a probe as a non-qualifying evidence source,"
      echo "and a work-bound breach with no probe-bound breach behind it as reopening a"
      echo "class-A ruling. Whether a probe that starved in one wait stands behind a work"
      echo "bound passed in another is not ruled, so this run is recorded under both and"
      echo "takes neither reading: the ledger rules on it before it is rerun."
    } >> "$summary"
  fi
}

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
  name_the_breached_bounds
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

name_the_breached_bounds

exit "$status"
