#!/usr/bin/env bash
#
# Read the pinned soak candidate and refuse everything a certification run
# could not be held to.
#
# **One parser for one format.** `.github/soak-candidate` holds `#` comment
# lines and exactly one line naming a 40-digit lowercase commit sha. That
# sentence is enforced here and asserted again by `norn --test certification`,
# and the two agree line for line: anything else — no line, two lines, an
# abbreviated sha, uppercase hex — is refused rather than concatenated into
# something that happens to parse.
#
# **What it verifies beyond the shape.** Three things, and each of them is a way
# a pointer edit that looks like every other pointer edit could hand the
# dispatcher something it should not run:
#
# 1. the sha names a commit this repository can fetch;
# 2. that commit is an ancestor of the default branch, so it is code that went
#    through review — GitHub serves any reachable sha, a fork's pull-request head
#    included, and the dispatcher builds the pinned tree;
# 3. the tree at it carries the certification workflow, so the pin is not older
#    than the mechanism.
#
# Each failure is an error here — in the dispatcher, which is a different
# workflow from the certification run and therefore never leaves a certification
# run without a record.
#
# The ancestry check needs history, so the caller's checkout is unshallow and
# nothing here fetches shallowly: a `--depth` fetch marks the clone shallow, and
# an ancestry question asked of a shallow clone is answered about the history
# that happened to be fetched.
#
# Writes `sha=<sha>` to the file `GITHUB_OUTPUT` names when that variable is
# set, and prints the sha on stdout either way.
#
# usage: soak-candidate.sh <pointer file> <workflow the pin must carry> <default branch>

set -euo pipefail

usage="usage: $0 <pointer file> <workflow the pin must carry> <default branch>"
pointer="${1:?$usage}"
mechanism="${2:?$usage}"
default="${3:?$usage}"

if [ ! -f "$pointer" ]; then
  echo "::error::${pointer} does not exist, so no candidate is pinned and there is nothing to certify" >&2
  exit 1
fi

named=$(grep -v '^[[:space:]]*#' "$pointer" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' | grep -v '^$' || true)
lines=0
if [ -n "$named" ]; then
  lines=$(printf '%s\n' "$named" | wc -l | tr -d '[:space:]')
fi
if [ "$lines" != "1" ]; then
  echo "::error::${pointer} holds ${lines} lines that are not comments, and the candidate is one sha" >&2
  exit 1
fi
sha="$named"
if ! printf '%s' "$sha" | grep -Eq '^[0-9a-f]{40}$'; then
  echo "::error::${pointer} names \`${sha}\`, which is not a 40-digit lowercase commit sha" >&2
  exit 1
fi

# Fetched rather than trusted. A typo, a commit off a fork, a branch that was
# force-pushed away and a commit nobody pushed all pass the shape check above
# and name no tree a run could be built from.
if ! git fetch --no-tags origin "$sha" >/dev/null 2>&1; then
  echo "::error::the pinned candidate ${sha} cannot be fetched from this repository, so no run can be built from it" >&2
  exit 1
fi
if ! git cat-file -e "${sha}^{commit}" 2>/dev/null; then
  echo "::error::the pinned candidate ${sha} is not a commit in this repository" >&2
  exit 1
fi

# **On the default branch, not merely reachable from it.** GitHub serves every
# sha reachable in the repository network, a fork's pull-request head included,
# and the dispatcher checks the pinned tree out and builds it. A pointer edit
# naming an unmerged sha looks like every other pointer edit, so the reviewed-ness
# the pointer's own prose assumes is checked here rather than assumed.
if ! git fetch --no-tags origin "+refs/heads/${default}:refs/remotes/origin/${default}" >/dev/null 2>&1; then
  echo "::error::the default branch ${default} could not be fetched, so the pinned candidate cannot be held to it" >&2
  exit 1
fi
if ! git merge-base --is-ancestor "$sha" "refs/remotes/origin/${default}"; then
  echo "::error::the pinned candidate ${sha} is not an ancestor of ${default}. The candidate is a reviewed commit on the default branch, and this run would otherwise build a tree that was never on it." >&2
  exit 1
fi

# **The pin has to carry the mechanism.** A certification run is the workflow
# file at the pin, so a pin older than that file would dispatch nothing — and a
# pin older than the rules a record is judged by would write a record of an
# earlier schema while the dispatcher claimed the current one.
if ! git cat-file -e "${sha}:${mechanism}" 2>/dev/null; then
  echo "::error::the pinned candidate ${sha} does not carry ${mechanism}, so it predates the certification mechanism and cannot be certified. Advance ${pointer} to a commit that carries it." >&2
  exit 1
fi

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "sha=${sha}" >> "$GITHUB_OUTPUT"
fi
printf '%s\n' "$sha"
