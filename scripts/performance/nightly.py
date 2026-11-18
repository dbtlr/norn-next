#!/usr/bin/env python3
"""Build independently receipted subjects, record the selected engines, and set the stored legacy record beside them."""
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

from legacy_record import STORED, key, mismatches
from provenance import sha256, verify_build
from record import fixture
from serve import other_servers, stop_group, stop_subjects

ENGINES = ("current", "serve", "legacy")

# How long one engine's record.py run may take, as seconds per process of each
# size: a fixed allowance plus one for every 1,000 background documents. These
# are runaway bounds, two to four times what completed hosted runs took per
# process (docs/ci.md lists them), so an engine that reaches one is stuck, not slow.
DEADLINES = {"current": (60, 10), "serve": (180, 10), "legacy": (600, 1600)}


def deadline(engine, sizes, processes):
    """The seconds one record.py run of `engine` may take over `sizes` and `processes`."""
    fixed, per_thousand = DEADLINES[engine]
    return processes * sum(fixed + per_thousand * size / 1000 for size in sizes)


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


def generate_fixtures(receipt, fixtures, sizes):
    """Generate each size's fixture with the receipted current worker; return each tree digest."""
    binary = Path(verify_build(receipt)["executable"])
    return {str(size): fixture(binary, fixtures, size)[1]["tree_sha256"] for size in sizes}


def stored_legacy(path, legacy, fixtures):
    """The stored legacy record and why it does not apply to this run, or None when none is stored.

    `legacy` is a callable returning the executable a legacy run would measure; a
    failure to obtain it is itself a reason, since the key cannot then be checked.
    """
    if not path.exists():
        return None, []
    stored = json.loads(path.read_text())
    try:
        digest = sha256(legacy())
    except Exception as problem:
        return stored, [f"the legacy executable could not be obtained to check the key: {problem!r}"]
    return stored, mismatches(stored, key(digest, fixtures))


def settings(record):
    sizes = " and ".join(f"{size:,}" for size in record.get("sizes", []))
    return (f"{sizes} background documents; {record.get('processes')} processes; "
            f"{record.get('samples')} samples after {record.get('warmups')} warmups; "
            f"writes after {record.get('write_payoff')}")


def provenance(engine, record, stored=False):
    executable = f"`{record.get('commit')}`, executable `{(record.get('build_sha256') or '')[:12]}`"
    if not stored:
        return (f"| {engine} | {executable}, compiler {record.get('compiler')} | "
                f"this run, {record.get('run_at')} (load {record.get('load_start')}) | {settings(record)} |")
    recorded = record.get("recorded") or {}
    where = f"[run {recorded['run_id']}]({recorded['url']}) at `{(recorded.get('workflow_sha') or '')[:12]}`" \
        if recorded.get("run_id") else "outside Actions"
    return (f"| {engine} (stored) | {executable} | {where}, {record.get('run_at')} to "
            f"{record.get('finished_at')} | {settings(record)} |")


def cell(row):
    if row is None:
        return "—"
    value = row["value"] if row["value"] is not None else "—"
    return value if row["status"] == "measured" else f"{value} ({row['status']})"


def shown(path):
    """A path as the repository names it, or its file name outside the repository."""
    try:
        return path.resolve().relative_to(Path(__file__).resolve().parents[2]).as_posix()
    except ValueError:
        return path.name


def summary(records, stored=None, reasons=(), stored_path=STORED):
    """The job summary: each record's provenance, the stored legacy record's state, and the metric table."""
    lines = ["## Nightly performance: " + ", ".join(records), "",
             "| Record | Executable | Recorded | Settings |", "|---|---|---|---|"]
    lines.extend(provenance(engine, record) for engine, record in records.items())
    columns = dict(records)
    if "legacy" not in records:
        if stored is not None:
            lines.append(provenance("legacy", stored, stored=True))
        lines.append("")
        if stored is None:
            lines.append(f"**No legacy record is stored** at `{shown(stored_path)}`, so legacy is not compared. "
                         "Record it with the legacy record workflow, as docs/performance.md describes.")
        elif reasons:
            lines.append("**The stored legacy record does not match this run, so its values are not shown:**")
            lines.extend(f"- {reason}" for reason in reasons)
            lines.append("Record legacy again with the legacy record workflow, as docs/performance.md describes.")
        else:
            sizes = " and ".join(f"{size:,}" for size in sorted({size for record in records.values()
                                                                 for size in record.get("sizes", [])}))
            lines.append(f"The stored legacy record matches this run: the same legacy executable, the same "
                         f"fixtures at {sizes} background documents, and record version {stored['record_version']}.")
            columns["legacy (stored)"] = stored
    lines.append("")
    indexed = {name: {(row["documents"], row["metric"]): row for row in record["rows"]} for name, record in columns.items()}
    measured_documents = {row["documents"] for record in records.values() for row in record["rows"]}
    order = []
    for name in columns:
        order.extend(item for item in indexed[name] if item not in order and item[0] in measured_documents)
    lines.append("| Documents | Metric | " + " | ".join(columns) + " | Unit | Note |")
    lines.append("|---:|---|" + "---:|" * len(columns) + "---|---|")
    for documents, metric in sorted(order, key=lambda item: item[0]):
        rows = [indexed[name].get((documents, metric)) for name in columns]
        present = next(row for row in rows if row is not None)
        notes = "; ".join(f"{name}: {row['note']}" for name, row in zip(columns, rows) if row and row["note"])
        lines.append(f"| {documents} | {metric} | " + " | ".join(str(cell(row)) for row in rows)
                     + f" | {present['unit']} | {notes} |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--engines", nargs="+", choices=ENGINES, default=list(ENGINES),
                        help="engines to record, each under its own deadline")
    parser.add_argument("--sizes", type=int, nargs="+", default=[2000, 10000])
    parser.add_argument("--legacy", type=Path)
    parser.add_argument("--stored-legacy", type=Path, default=STORED,
                        help="stored legacy record set beside the engines when legacy is not recorded")
    parser.add_argument("--target-directory", type=Path)
    parser.add_argument("--processes", type=int, default=3)
    parser.add_argument("--samples", type=int, default=400)
    parser.add_argument("--warmups", type=int, default=5)
    parser.add_argument("--write-payoff", type=int, default=200)
    args = parser.parse_args()
    engines = [engine for engine in ENGINES if engine in args.engines]
    signal.signal(signal.SIGTERM, terminated)
    if other_servers():
        raise ValueError(f"quiet-host check found other norn serve processes: {other_servers()}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    def package():
        paths = [*output.glob("*.json"), *output.glob("*.jsonl"), *output.glob("*.log"), *output.glob("*.md")]
        paths.extend(output.glob("fixtures/*/manifest.json"))
        for engine in ENGINES:
            paths.extend((output / engine).glob("record.json"))
            paths.extend((output / engine / "work").glob("*/worker.*.log"))
        with tarfile.open(output / "evidence.tar.gz", "w:gz") as tar:
            for path in paths:
                tar.add(path, arcname=str(path.relative_to(output)))
    atexit.register(package)
    here = Path(__file__).resolve().parent
    target = args.target_directory or output / "build"
    # Every engine needs the fixtures, and only the current worker generates them.
    builds = [("current", "--startup-profile")] + ([("serve", "--serve")] if "serve" in engines else [])
    for engine, flag in builds:
        run(here / "provenance.py", flag, "--target-directory", target, "--receipt", output / f"{engine}-build.json")
    fixtures = generate_fixtures(output / "current-build.json", output / "fixtures", args.sizes)
    legacy = None
    def legacy_executable():
        nonlocal legacy
        legacy = legacy or (args.legacy.resolve() if args.legacy else legacy_binary(output))
        return legacy
    for engine in engines:
        identity = ["--legacy", legacy_executable()] if engine == "legacy" else ["--serve-build" if engine == "serve" else "--current-build", output / f"{engine}-build.json"]
        run(here / "record.py", "--engine", engine, *identity,
            "--fixtures", output / "fixtures", "--output", output / engine,
            "--sizes", *args.sizes, "--processes", args.processes, "--samples", args.samples,
            "--warmups", args.warmups, "--write-payoff", args.write_payoff, subjects=output / engine,
            timeout=deadline(engine, args.sizes, args.processes))
    records = {engine: json.loads((output / engine / "record.json").read_text()) for engine in engines}
    stored, reasons = (None, []) if "legacy" in engines else stored_legacy(args.stored_legacy, legacy_executable, fixtures)
    text = summary(records, stored, reasons, args.stored_legacy)
    (output / "summary.md").write_text(text)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as file:
            file.write(text)
    if "legacy" not in engines and os.environ.get("GITHUB_ACTIONS"):
        state = "none is stored" if stored is None else "; ".join(reasons) if reasons else None
        if state:
            print(f"::warning title=Stored legacy record::Legacy is not compared: {state}", flush=True)
    print(output / "summary.md")


if __name__ == "__main__":
    main()
