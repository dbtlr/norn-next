#!/usr/bin/env bash
#
# Run what PR CI runs on Linux, locally, and say which step failed.
#
# The gate is every `run:` step of every Linux job (`runs-on: ubuntu-*`) of
# .github/workflows/ci.yml, in file order, each with the environment the
# workflow layers over it (workflow `env`, then job `env`, then step `env`).
# `uses:` steps (checkout, the cache, the actionlint container) are skipped, and
# so is every job on another runner, the macOS job among them. No command is
# written in this script: the `norn-gate-steps` binary of norn-testkit reads the
# workflow through the reader the CI guards use, so a step added to or removed
# from ci.yml changes what runs here with no edit to this file. A Linux step the
# gate cannot honor as CI does (an `if:`, a `working-directory`, a `${{ }}`
# expression) stops the gate with an error rather than running differently.
#
# It does not run the nightly lanes (counter_gate, the compiled audits, the
# fault lanes): regression.yml owns them. See docs/ci.md, "Local runs".
#
# usage: scripts/gate.sh [--list] [--job <job-key>] [--step <number|name>]
#                        [--workflow <file>]
#
#   --list      print the steps that would run, without running any
#   --job       run only the steps of one job, by its key under `jobs`
#   --step      run only one step: its number in the --list output, or its
#               exact name (with --job, the number counts within that job)
#   --workflow  read another workflow file (default .github/workflows/ci.yml)
#
# Every step runs to completion even after an earlier one fails. Each prints its
# job and name, then PASS or FAIL with its exit status and wall time, and the
# gate ends with a summary of every step and exits non-zero if any failed. A
# step runs in its own `bash -eo pipefail` with only its own workflow
# environment added to the caller's, so nothing leaks from one step to the next.
#
# Machine policy belongs to the caller, not to this script. It writes no
# absolute path and takes no lock. The caller supplies CARGO_TARGET_DIR, TMPDIR,
# CARGO_BUILD_JOBS, the toolchain on PATH, and any lock that serializes heavy
# cargo runs, by wrapping the whole script, for example
#   flock <lockfile> scripts/gate.sh
# The helper that reads the workflow is built with cargo under that same
# environment.
#
# What a caller must not set. The measurement suites inside the gate (the lint
# job's counter bars and serve_memory lane, and the baselines of the host and
# fixture suites) refuse a build that is not the opt-level 0 subject their bars
# were authored on. See crates/norn-testkit/src/build_subject/. So the caller
# sets none of:
#   - any CARGO_PROFILE_* variable, including CARGO_PROFILE_DEV_DEBUG and
#     CARGO_PROFILE_TEST_DEBUG, which the gate refuses at start;
#   - RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS carrying an optimization flag;
#   - a `--config` argument or a cargo config file holding a profile table
#     (the one `--config` the host functional step carries comes from the
#     workflow and is the build-profile guard's accepted exception);
#   - a relative CARGO_HOME, or a RUSTC_WRAPPER that alters code generation.
# CARGO_INCREMENTAL=0 is safe and shrinks the target directory.

set -uo pipefail

workflow=.github/workflows/ci.yml
list_only=0
only_job=
only_step=

usage() {
  sed -n '/^# usage:/,/^#   --workflow/p' "$0" | sed 's/^# \{0,1\}//' >&2
}

while [ "$#" -gt 0 ]; do
  case $1 in
    --list) list_only=1 ;;
    --job) only_job=${2-}; shift ;;
    --step) only_step=${2-}; shift ;;
    --workflow) workflow=${2-}; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "gate: unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

if ((BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4))); then
  echo "gate: bash 4.4 or newer is required (mapfile -d)" >&2
  exit 2
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2

if [ "$list_only" -eq 0 ]; then
  profile_overrides=$(compgen -e | grep '^CARGO_PROFILE_' || true)
  if [ -n "$profile_overrides" ]; then
    echo "gate: the measurement suites refuse a profile override; unset:" >&2
    echo "$profile_overrides" >&2
    exit 2
  fi
fi

# The workflow's steps, as NUL-delimited records (see norn-gate-steps).
records=$(mktemp) || exit 2
trap 'rm -f "$records"' EXIT
if ! cargo run --locked --quiet -p norn-testkit --bin norn-gate-steps -- "$workflow" >"$records"; then
  echo "gate: could not read the steps of $workflow" >&2
  exit 2
fi
mapfile -d '' -t fields <"$records"

# Index every step: job key, job name, step name, and where its record starts.
jobs=()
job_names=()
names=()
starts=()
at=0
while [ "$at" -lt "${#fields[@]}" ]; do
  env_count=${fields[at + 3]}
  jobs+=("${fields[at]}")
  job_names+=("${fields[at + 1]}")
  names+=("${fields[at + 2]}")
  starts+=("$at")
  at=$((at + 5 + env_count))
done

# The steps this invocation selects, numbered as --list numbers them.
selected=()
within_job=0
for i in "${!starts[@]}"; do
  [ -z "$only_job" ] || [ "${jobs[i]}" = "$only_job" ] || continue
  within_job=$((within_job + 1))
  if [ -n "$only_step" ]; then
    case $only_step in
      *[!0-9]*) [ "${names[i]}" = "$only_step" ] || continue ;;
      *) [ "$within_job" -eq "$only_step" ] || continue ;;
    esac
  fi
  selected+=("$i")
done

if [ "${#selected[@]}" -eq 0 ]; then
  echo "gate: no step matches --job '${only_job}' --step '${only_step}'" >&2
  exit 2
fi

# Print the command of step $1, indented.
show_command() {
  local start=${starts[$1]}
  local command=${fields[start + 4 + ${fields[start + 3]}]}
  printf '%s\n' "${command%$'\n'}" | sed 's/^/    /'
}

if [ "$list_only" -eq 1 ]; then
  number=0
  for i in "${selected[@]}"; do
    number=$((number + 1))
    printf '%s (%s) / %s\n' "${jobs[i]}" "${job_names[i]}" "${names[i]}"
    start=${starts[i]}
    for ((e = 0; e < fields[start + 3]; e++)); do
      printf '    env %s\n' "${fields[start + 4 + e]}"
    done
    show_command "$i"
  done
  exit 0
fi

results=()
failed=0
gate_started=$SECONDS
number=0
for i in "${selected[@]}"; do
  number=$((number + 1))
  start=${starts[i]}
  env_count=${fields[start + 3]}
  assignments=("${fields[@]:start + 4:env_count}")
  command=${fields[start + 4 + env_count]}
  label="${jobs[i]} / ${names[i]}"

  printf '\n===== [%d/%d] %s =====\n' "$number" "${#selected[@]}" "$label"
  show_command "$i"
  began=$SECONDS
  # A subshell-free `env` keeps the step's variables out of this shell and of
  # every later step.
  env "${assignments[@]}" bash --noprofile --norc -eo pipefail -c "$command"
  status=$?
  took=$((SECONDS - began))
  if [ "$status" -eq 0 ]; then
    verdict=PASS
  else
    verdict=FAIL
    failed=$((failed + 1))
  fi
  printf '===== %s %s (exit %d, %ds) =====\n' "$verdict" "$label" "$status" "$took"
  results+=("$verdict exit=$status ${took}s  $label")
done

printf '\n===== summary: %d step(s), %d failed, %ds =====\n' \
  "${#selected[@]}" "$failed" "$((SECONDS - gate_started))"
printf '%s\n' "${results[@]}"

[ "$failed" -eq 0 ]
