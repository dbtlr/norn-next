#!/usr/bin/env bash
#
# Take this runner's host-health reading, before the lane builds anything.
#
# The classification lives in
# `norn_testkit::certification::preflight` and this script does not do any of
# it: what it writes is the measurement, on the one line that module's
# `Reading::render`/`Reading::parse` agree on, and the verdict is reached later
# by the classifier the lane runs.
#
# **It runs before the build for the reason the split exists.** The reading the
# preflight turns on is how busy the machine was when the lane landed on it. A
# cold cargo build is minutes of every core, so a load average read after one is
# a reading of this run's own compile — and every lane would refuse itself.
#
# The output is the `NORN_PREFLIGHT_READING=<line>` assignment a lane appends to
# `$GITHUB_ENV`, so a step that measures nothing still writes a variable and the
# classifier records "not measured" rather than falling back to reading the
# host at the wrong moment.
#
# usage: host-readings.sh >> "$GITHUB_ENV"

set -uo pipefail

reading=""

cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)
case "$cores" in
  ''|*[!0-9]*) ;;
  *) reading="cores=${cores}" ;;
esac

# The one-minute average, in thousandths: an integer the record is comparable
# on years later, where a float is a rendering choice.
one_minute=""
if [ -r /proc/loadavg ]; then
  one_minute=$(cut -d' ' -f1 < /proc/loadavg)
elif command -v sysctl >/dev/null 2>&1; then
  # `{ 1.23 4.56 7.89 }`, most recent first.
  one_minute=$(sysctl -n vm.loadavg 2>/dev/null | tr -d '{}' | awk '{print $1}')
fi
if printf '%s\n' "$one_minute" | grep -qE '^[0-9]+(\.[0-9]+)?$'; then
  reading="${reading} load-milli=$(awk -v load="$one_minute" 'BEGIN { printf "%d", load * 1000 }')"
fi

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
