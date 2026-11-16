#!/usr/bin/env python3
"""Build independently receipted subjects and record all three engines serially."""
import argparse
import atexit
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tarfile
import urllib.request

from serve import other_servers, stop_group, stop_subjects


def run(*arguments, subjects=None, timeout=1800):
    """Run a script in a session of its own, and clean up after it however it ends.

    A script still running here, timed out or with the controller interrupted, is
    stopped with its whole process group: SIGTERM, then SIGKILL. Every serving
    subject a note under `subjects` names is stopped too, since a subject leads a
    session of its own and outlives a recorder killed before it could close it.
    """
    process = subprocess.Popen([sys.executable, *map(str, arguments)], start_new_session=True)
    try:
        status = process.wait(timeout=timeout)
    finally:
        # The whole group is stopped, not only its leader: a leader that exits
        # on SIGTERM can leave members that ignore it. The group id is the
        # leader's pid, and stays taken while the leader is unreaped or any
        # member lives (stop_group states the one window left).
        stop_group(process.pid, reap=process.poll)
        process.wait()
        stop_group(process.pid)
        if subjects is not None:
            stop_subjects(subjects)
    if status != 0:
        raise subprocess.CalledProcessError(status, process.args)


def terminated(number, frame):
    """A SIGTERM ends the controller as an exception does, so its cleanup runs."""
    raise SystemExit(128 + number)


def legacy_binary(output):
    """Freeze the version-pinned legacy release, checking its published digest."""
    name = "norn-run-x86_64-unknown-linux-musl.tar.xz"
    base = "https://github.com/dbtlr/norn/releases/download/v0.48.3/"
    archive = output / name
    with urllib.request.urlopen(base + name, timeout=60) as source:
        archive.write_bytes(source.read())
    with urllib.request.urlopen(base + name + ".sha256", timeout=60) as source:
        expected = source.read().decode().split()[0]
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise ValueError("legacy release digest mismatch")
    with tarfile.open(archive) as tar:
        member = next(item for item in tar.getmembers() if item.isfile() and Path(item.name).name in ("norn", "norn-run"))
        binary = output / "legacy-norn"
        binary.write_bytes(tar.extractfile(member).read())
        binary.chmod(0o700)
    return binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sizes", type=int, nargs="+", default=[2000, 10000])
    parser.add_argument("--legacy", type=Path)
    parser.add_argument("--target-directory", type=Path)
    parser.add_argument("--processes", type=int, default=3)
    parser.add_argument("--samples", type=int, default=400)
    parser.add_argument("--warmups", type=int, default=5)
    parser.add_argument("--write-payoff", type=int, default=200)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, terminated)
    if other_servers():
        raise ValueError(f"quiet-host check found other norn serve processes: {other_servers()}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    def package():
        paths = [*output.glob("*.json"), *output.glob("*.jsonl"), *output.glob("*.log"), *output.glob("*.md")]
        for engine in ("current", "serve", "legacy"):
            paths.extend((output / engine).glob("record.json"))
            paths.extend((output / engine / "work").glob("*/worker.*.log"))
        with tarfile.open(output / "evidence.tar.gz", "w:gz") as tar:
            for path in paths:
                tar.add(path, arcname=str(path.relative_to(output)))
    atexit.register(package)
    here = Path(__file__).resolve().parent
    for engine, flag in [("current", "--startup-profile"), ("serve", "--serve")]:
        run(here / "provenance.py", flag, "--target-directory", args.target_directory or output / "build", "--receipt", output / f"{engine}-build.json")
    legacy = args.legacy.resolve() if args.legacy else legacy_binary(output)
    for engine in ("current", "serve", "legacy"):
        identity = ["--legacy", legacy] if engine == "legacy" else ["--serve-build" if engine == "serve" else "--current-build", output / f"{engine}-build.json"]
        run(here / "record.py", "--engine", engine, *identity,
            "--fixtures", output / "fixtures", "--output", output / engine,
            "--sizes", *args.sizes, "--processes", args.processes, "--samples", args.samples,
            "--warmups", args.warmups, "--write-payoff", args.write_payoff, subjects=output / engine)
    records = {engine: json.loads((output / engine / "record.json").read_text()) for engine in ("current", "serve", "legacy")}
    summary = ["| Engine | Documents | Metric | Value | Unit | Note |", "|---|---:|---|---:|---|---|"]
    for engine, record in records.items():
        summary.extend(f'| {engine} | {row["documents"]} | {row["metric"]} | {row["value"]} | {row["unit"]} | {row["note"]} |' for row in record["rows"])
    text = "\n".join(summary) + "\n"
    (output / "summary.md").write_text(text)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as file:
            file.write(text)
    print(output / "summary.md")


if __name__ == "__main__":
    main()
