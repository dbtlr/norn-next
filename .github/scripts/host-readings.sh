#!/usr/bin/env bash
#
# Take this runner's host-health reading, before the lane builds anything.
#
# The classification lives in `norn_testkit::certification::preflight` and this
# script does none of it: what it writes is the measurement, on the one line
# that module's `Reading::render`/`Reading::parse` agree on, and the verdict is
# reached later by the classifier the lane runs.
#
# **It runs before the build for the reason the split exists.** What the
# preflight turns on is how much of the machine was already spoken for when the
# lane landed on it. A cold cargo build is minutes of every processor, so a
# share sampled after one is a reading of this run's own compile — and every
# lane would refuse itself.
#
# **The share is sampled over a window rather than read off the load average.**
# A load average is decayed history, and a runner is handed over minutes after
# its own boot: a `macos-15` runner reads a one-minute average of 11.6 over
# three cores while sitting idle. The window is about the machine the suites are
# about to get.
#
# The output is the `NORN_PREFLIGHT_READING=<line>` assignment a lane appends to
# `$GITHUB_ENV`, so a step that measures nothing still writes a variable and the
# classifier records "not measured" rather than falling back to reading the host
# at the wrong moment.
#
# usage: host-readings.sh >> "$GITHUB_ENV"

set -uo pipefail

reading=""

cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)
case "$cores" in
  ''|*[!0-9]*) ;;
  *) reading="cores=${cores}" ;;
esac

# Busy share in tenths of a percent of the whole machine: an integer the record
# is comparable on years later, where a float is a rendering choice.
busy=""
if [ -r /proc/stat ]; then
  # Settled first, so the window is the machine rather than the tail of the
  # checkout that just finished on it.
  sleep 2
  first=$(awk '$1 == "cpu" { $1 = ""; print; exit }' /proc/stat)
  sleep 2
  second=$(awk '$1 == "cpu" { $1 = ""; print; exit }' /proc/stat)
  busy=$(awk -v a="$first" -v b="$second" '
    BEGIN {
      na = split(a, x, " "); nb = split(b, y, " ")
      if (na < 5 || nb < 5) exit
      for (i = 1; i <= nb; i++) { total += y[i]; if (i <= na) total -= x[i] }
      # user nice system idle iowait ...; a processor blocked on the disk is one
      # the suites can have, so iowait counts as idle.
      idle = (y[4] + y[5]) - (x[4] + x[5])
      if (total <= 0) exit
      share = 1000 - (idle * 1000 / total)
      if (share < 0) share = 0
      if (share > 1000) share = 1000
      printf "%d", share
    }')
elif command -v top >/dev/null 2>&1; then
  # `CPU usage: 5.12% user, 8.20% sys, 86.67% idle`. The first line top prints
  # is since boot, and the last is a window that opened two samples in — the
  # settle and the window in one invocation.
  busy=$(top -l 3 -n 0 -s 2 2>/dev/null | awk '
    /^CPU usage:/ { last = $0 }
    END {
      if (last == "") exit
      n = split(last, parts, ",")
      for (i = 1; i <= n; i++) {
        if (parts[i] ~ /idle/) {
          gsub(/[^0-9.]/, "", parts[i])
          share = 1000 - (parts[i] * 10)
          if (share < 0) share = 0
          if (share > 1000) share = 1000
          printf "%d", share
        }
      }
    }')
fi
case "$busy" in
  ''|*[!0-9]*) ;;
  *) reading="${reading} busy-deci=${busy}" ;;
esac

# Darwin only: the real-watcher cases subscribe to this daemon, and a saturated
# or bloated one delivers late enough to spend a case's whole work bound.
if [ "$(uname -s)" = Darwin ]; then
  daemon=$(ps -Axo '%cpu=,rss=,comm=' 2>/dev/null | awk '$3 ~ /\/fseventsd$|^fseventsd$/ { print $1, $2; exit }')
  if [ -z "$daemon" ]; then
    if ps -Axo comm= >/dev/null 2>&1; then
      reading="${reading} fseventsd=absent"
    else
      reading="${reading} fseventsd=unread"
    fi
  else
    cpu=$(printf '%s\n' "$daemon" | awk '{ printf "%d", $1 * 10 }')
    rss=$(printf '%s\n' "$daemon" | awk '{ printf "%d", $2 }')
    reading="${reading} fseventsd=running fseventsd-cpu-deci=${cpu} fseventsd-rss-kib=${rss}"
  fi
fi

# Trimmed, and written whatever it holds: an empty reading is a host nobody
# measured, which the classifier refuses on.
printf 'NORN_PREFLIGHT_READING=%s\n' "$(printf '%s' "$reading" | sed 's/^ *//; s/ *$//')"
