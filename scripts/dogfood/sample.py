#!/usr/bin/env python3
"""The dogfood sampler: one hourly reading of a running `norn serve`, and the report over them.

    sample.py            append one JSON line describing the host
    sample.py report     read the lines back and say whether memory or descriptors are growing

A sample line is `{"at", "pid", "build", "uptime_s", "rss_bytes", "fds", "threads"}`
appended to `$XDG_STATE_HOME/norn-dogfood/samples.jsonl` (`~/.local/state` when
`XDG_STATE_HOME` is unset). The process figures come from `/proc` on Linux and from
`ps` and `lsof` on macOS. The build comes from the host's hello, dialed with the
dogfood token and verified as every client verifies it (ADR 0054), so a sample is
never taken from a process that merely holds the port.

This is monitoring, not a gate. `report` exits 1 when a threshold is crossed so a
timer or a person notices; the defect it calls for is filed by hand.
"""

import argparse
import http.client
import json
import os
import re
import secrets
import statistics
import subprocess
import sys
import tomllib
from pathlib import Path

# The hello is spoken once, by the performance harness's serving engine; reuse it.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "performance"))
from serve import socket_address, verify_hello  # noqa: E402

DAY_SECONDS = 86_400
# Descriptors above day 1's maximum plus this many call for a defect.
FD_HEADROOM = 8
# A resident-memory slope of this share of day 1's median, per day, calls for one.
SLOPE_SHARE_PER_DAY = 0.01
# The channels' default ports, as `norn-config` fixes them.
CHANNELS = {"dev": ("norn-dev", 28472), "live": ("norn", 28471)}
HOST = "127.0.0.1"
HELLO_TIMEOUT_S = 10

FIELDS = ("at", "pid", "build", "uptime_s", "rss_bytes", "fds", "threads")


# ---- Locations --------------------------------------------------------------


def state_file(environ=os.environ):
    """`$XDG_STATE_HOME/norn-dogfood/samples.jsonl`, with the XDG default under HOME."""
    base = environ.get("XDG_STATE_HOME")
    root = Path(base) if base and os.path.isabs(base) else Path.home() / ".local/state"
    return root / "norn-dogfood" / "samples.jsonl"


def config_dir(channel, environ=os.environ):
    """The channel's configuration directory, as `norn-config` lays it out."""
    base = environ.get("XDG_CONFIG_HOME")
    root = Path(base) if base and os.path.isabs(base) else Path.home() / ".config"
    return root / CHANNELS[channel][0]


def token_file(channel, environ=os.environ):
    """The channel's token file, as `norn-config` lays it out."""
    return config_dir(channel, environ) / "tokens.toml"


def channel_port(channel, environ=os.environ):
    """The port the channel's host listens on: `port` under `[endpoint]` in the
    channel's `config.toml` when set there, else the channel's default, as
    `norn serve` resolves it without `--port`. A file the host would refuse is
    left to the host; the sampler falls back to the default and the dial says so."""
    try:
        settings = tomllib.loads((config_dir(channel, environ) / "config.toml").read_text())
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
        return CHANNELS[channel][1]
    endpoint = settings.get("endpoint")
    port = endpoint.get("port") if isinstance(endpoint, dict) else None
    # `type(...) is int`, not isinstance: TOML's `true` reads as a bool, and a
    # bool is an int to isinstance.
    return port if type(port) is int and 1 <= port <= 65535 else CHANNELS[channel][1]


def read_secret(path, label):
    """The raw bytes of the token `label` in the file at `path` (stored as lowercase hex)."""
    tokens = tomllib.loads(Path(path).read_text()).get("tokens", {})
    if label not in tokens:
        raise ValueError(f"no token labelled {label!r} in {path}; add one as docs/mcp.md describes")
    return bytes.fromhex(tokens[label]["secret"])


# ---- The hello --------------------------------------------------------------


def dial_hello(port, label, secret, channel):
    """Speak the hello on a fresh connection and return the verified answer.

    Raises when the host does not prove itself under the token (ADR 0054); no
    bearer is ever sent, because the hello is the only exchange this makes.
    """
    connection = http.client.HTTPConnection(HOST, port, timeout=HELLO_TIMEOUT_S)
    try:
        connection.connect()
        local = connection.sock.getsockname()
        nonce = secrets.token_bytes(32)
        body = json.dumps({"label": label, "nonce": nonce.hex()})
        connection.request("POST", "/v1/hello", body, {"Content-Type": "application/json"})
        response = connection.getresponse()
        answer = json.loads(response.read())
        verify_hello(response.status, answer, nonce=nonce, secret=secret, channel=channel,
                     host=socket_address(HOST, port), client=socket_address(*local[:2]))
        return answer
    finally:
        connection.close()


# ---- Process figures --------------------------------------------------------


def run_text(argv):
    return subprocess.run(argv, check=True, capture_output=True, text=True).stdout


def parse_etime(text):
    """Seconds in `ps`'s elapsed form `[[dd-]hh:]mm:ss`."""
    days, _, clock = text.strip().rpartition("-")
    parts = [int(part) for part in clock.split(":")]
    while len(parts) < 3:
        parts.insert(0, 0)
    hours, minutes, seconds = parts
    return (int(days or 0) * 24 + hours) * 3600 + minutes * 60 + seconds


def parse_proc_status(text):
    """`(rss_bytes, threads)` from the text of `/proc/<pid>/status`."""
    rss = threads = None
    for line in text.splitlines():
        name, _, value = line.partition(":")
        if name == "VmRSS":
            rss = int(value.split()[0]) * 1024
        elif name == "Threads":
            threads = int(value)
    if rss is None or threads is None:
        raise ValueError("process status holds no VmRSS or Threads line")
    return rss, threads


def proc_start_ticks(stat_text):
    """The process's start time in clock ticks after boot, from `/proc/<pid>/stat`."""
    return int(stat_text.rsplit(")", 1)[1].split()[19])


def linux_figures(pid, proc=Path("/proc")):
    base = proc / str(pid)
    rss, threads = parse_proc_status((base / "status").read_text())
    ticks = os.sysconf("SC_CLK_TCK")
    booted_for = float((proc / "uptime").read_text().split()[0])
    uptime = booted_for - proc_start_ticks((base / "stat").read_text()) / ticks
    return {"uptime_s": int(uptime), "rss_bytes": rss, "fds": len(list((base / "fd").iterdir())),
            "threads": threads}


def macos_figures(pid):
    rss_kib, etime = run_text(["ps", "-o", "rss=,etime=", "-p", str(pid)]).split()
    # `ps -M` prints a header and one row per thread.
    threads = len(run_text(["ps", "-M", "-p", str(pid)]).splitlines()) - 1
    fds = count_lsof_descriptors(run_text(["lsof", "-n", "-P", "-p", str(pid)]))
    return {"uptime_s": parse_etime(etime), "rss_bytes": int(rss_kib) * 1024, "fds": fds,
            "threads": threads}


def count_lsof_descriptors(text):
    """The open descriptors in `lsof -p` output.

    `lsof` also lists references that are not descriptors, such as `cwd`, `txt`
    and `mem`; only an FD column that starts with a digit (`3u`, `12r`) names one.
    """
    rows = text.splitlines()[1:]
    return sum(1 for row in rows if len(row.split()) > 3 and row.split()[3][:1].isdigit())


def process_figures(pid):
    return macos_figures(pid) if sys.platform == "darwin" else linux_figures(pid)


def listener_pid(port):
    """The process listening on `HOST:port`, the address the hello was dialed on,
    which the hello cannot name. Another address on the same port is a different
    listener, so the address must match as well as the port."""
    if sys.platform == "darwin":
        out = run_text(["lsof", "-n", "-P", f"-iTCP@{HOST}:{port}", "-sTCP:LISTEN", "-t"])
        return int(out.split()[0])
    return linux_listener_pid(port)


def proc_net_address(ip, port):
    """An IPv4 `ip:port` as `/proc/net/tcp` writes a local address: the address's
    bytes in host (little-endian) order, then the port, both in upper-case hex."""
    octets = [int(part) for part in ip.split(".")]
    return "".join(f"{octet:02X}" for octet in reversed(octets)) + f":{port:04X}"


def linux_listener_pid(port, proc=Path("/proc"), ip=HOST):
    """Find the socket listening on exactly `ip:port` in `/proc/net/tcp`, then the
    process holding it."""
    wanted = proc_net_address(ip, port)
    inodes = set()
    try:
        rows = (proc / "net" / "tcp").read_text().splitlines()[1:]
    except FileNotFoundError:
        rows = []
    for row in rows:
        fields = row.split()
        local, state, inode = fields[1], fields[3], fields[9]
        if state == "0A" and local == wanted:
            inodes.add(inode)
    targets = {f"socket:[{inode}]" for inode in inodes}
    for fd_dir in proc.glob("[0-9]*/fd"):
        try:
            if any(os.readlink(fd) in targets for fd in fd_dir.iterdir()):
                return int(fd_dir.parent.name)
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            continue
    raise LookupError(f"no process of this user listens on {ip}:{port}")


# ---- The record -------------------------------------------------------------


def build_record(now, pid, build, figures):
    """One sample line's object, its keys in the documented order."""
    record = {"at": int(now), "pid": pid, "build": build, **figures}
    return {name: record[name] for name in FIELDS}


def append_record(path, record):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as out:
        out.write(json.dumps(record, separators=(",", ":")) + "\n")


def load_samples(path):
    """Every well-formed sample line in `path`; a torn or foreign line is skipped."""
    samples = []
    for line in Path(path).read_text().splitlines():
        try:
            sample = json.loads(line)
        except ValueError:
            continue
        if isinstance(sample, dict) and all(name in sample for name in FIELDS):
            samples.append(sample)
    return sorted(samples, key=lambda sample: sample["at"])


# ---- The report -------------------------------------------------------------


def current_run(samples):
    """The trailing samples of the latest host: the same build and the same process.

    A restart, or a new build, begins a new run, since its memory starts over.
    """
    if not samples:
        return []
    last = samples[-1]
    run = []
    uptime = last["uptime_s"]
    for sample in reversed(samples):
        if (sample["build"], sample["pid"]) != (last["build"], last["pid"]):
            break
        if sample["uptime_s"] > uptime:
            break
        run.append(sample)
        uptime = sample["uptime_s"]
    return run[::-1]


def rss_slope_per_day(samples):
    """The least-squares slope of RSS, in bytes per day, or None below two distinct times."""
    start = samples[0]["at"]
    days = [(sample["at"] - start) / DAY_SECONDS for sample in samples]
    if len(set(days)) < 2:
        return None
    return statistics.linear_regression(days, [sample["rss_bytes"] for sample in samples]).slope


def analyze(samples):
    """The report's figures and which thresholds the run crosses.

    Day 1 is the first 24 hours of the run, and the latest day the last 24. The slope is fitted over the whole run,
    and is judged only once the run spans a day, since a shorter fit says little.
    """
    if not samples:
        raise ValueError("no samples")
    start = samples[0]["at"]
    first = [s for s in samples if s["at"] - start < DAY_SECONDS]
    # The latest day is the 24 hours ending at the last sample, so it is never a thin calendar bucket.
    latest = [s for s in samples if s["at"] > samples[-1]["at"] - DAY_SECONDS]
    day1_fds_max = max(s["fds"] for s in first)
    day1_rss_median = statistics.median(s["rss_bytes"] for s in first)
    slope = rss_slope_per_day(samples)
    matured = samples[-1]["at"] - start >= DAY_SECONDS
    fd_limit = day1_fds_max + FD_HEADROOM
    slope_limit = SLOPE_SHARE_PER_DAY * day1_rss_median
    latest_fds_max = max(s["fds"] for s in latest)
    return {
        "build": samples[-1]["build"], "pid": samples[-1]["pid"], "samples": len(samples),
        "span_s": samples[-1]["at"] - start,
        "day1": {"samples": len(first), "fds_max": day1_fds_max, "rss_median": day1_rss_median},
        "latest_day": {"samples": len(latest), "fds_max": latest_fds_max,
                       "rss_median": statistics.median(s["rss_bytes"] for s in latest),
                       "rss_last": samples[-1]["rss_bytes"]},
        "rss_slope_per_day": slope, "slope_limit_per_day": slope_limit, "fd_limit": fd_limit,
        "slope_judged": matured and slope is not None,
        "crossed": {"descriptors": latest_fds_max > fd_limit,
                    "rss_slope": matured and slope is not None and slope >= slope_limit},
    }


def format_report(result):
    mib = 1 << 20
    day1, latest = result["day1"], result["latest_day"]
    lines = [
        f"build {result['build']} (pid {result['pid']}): {result['samples']} samples over "
        f"{result['span_s'] / 3600:.1f} h",
        f"day 1:      max descriptors {day1['fds_max']}, median RSS {day1['rss_median'] / mib:.1f} MiB "
        f"({day1['samples']} samples)",
        f"last 24 h: max descriptors {latest['fds_max']}, median RSS "
        f"{latest['rss_median'] / mib:.1f} MiB, last RSS {latest['rss_last'] / mib:.1f} MiB "
        f"({latest['samples']} samples)",
    ]
    slope = result["rss_slope_per_day"]
    if slope is None:
        lines.append("RSS slope:  not enough samples to fit")
    else:
        note = "" if result["slope_judged"] else " (run is under a day: not judged)"
        lines.append(f"RSS slope:  {slope / mib:+.3f} MiB/day, threshold "
                     f"{result['slope_limit_per_day'] / mib:.3f} MiB/day{note}")
    lines.append(f"descriptors: threshold {result['fd_limit']} (day 1 max + {FD_HEADROOM})")
    crossed = [name for name, hit in result["crossed"].items() if hit]
    lines.append("THRESHOLD CROSSED: " + ", ".join(crossed) + ". File a defect against the current phase."
                 if crossed else "No threshold crossed.")
    return "\n".join(lines)


# ---- Commands ---------------------------------------------------------------


def sample_command(args, clock):
    port = args.port or channel_port(args.channel)
    secret = read_secret(args.token_file or token_file(args.channel), args.label)
    answer = dial_hello(port, args.label, secret, args.channel)
    pid = args.pid or listener_pid(port)
    record = build_record(clock(), pid, answer["build"], process_figures(pid))
    append_record(args.samples or state_file(), record)
    print(json.dumps(record))
    return 0


def report_command(args):
    path = args.samples or state_file()
    samples = current_run(load_samples(path)) if Path(path).exists() else []
    if not samples:
        print(f"no samples in {path}", file=sys.stderr)
        return 2
    result = analyze(samples)
    print(format_report(result))
    return 1 if any(result["crossed"].values()) else 0


def parser():
    top = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    top.add_argument("--samples", type=Path, help="the samples file (default: under $XDG_STATE_HOME)")
    sub = top.add_subparsers(dest="command")
    report = sub.add_parser("report", help="judge the samples; exit 1 when a threshold is crossed")
    # SUPPRESS keeps the subparser from overwriting a value given before the subcommand.
    report.add_argument("--samples", type=Path, default=argparse.SUPPRESS, help="the samples file")
    top.add_argument("--channel", choices=sorted(CHANNELS), default="dev", help="default: dev")
    top.add_argument("--label", default="dogfood", help="the token label to dial with (default: dogfood)")
    top.add_argument("--port", type=int, help="the host's port (default: [endpoint] port in the channel's config.toml, else the channel's default)")
    top.add_argument("--token-file", type=Path, help="default: the channel's under $XDG_CONFIG_HOME")
    top.add_argument("--pid", type=int, help="the host's pid (default: whoever listens on the port)")
    return top


def main(argv=None, clock=None):
    import time
    args = parser().parse_args(argv)
    try:
        if args.command == "report":
            return report_command(args)
        return sample_command(args, clock or time.time)
    except (ValueError, KeyError, LookupError, OSError, subprocess.CalledProcessError) as error:
        print(f"sample.py: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
