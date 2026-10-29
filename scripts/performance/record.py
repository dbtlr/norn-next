#!/usr/bin/env python3
"""Record one timing run of a single engine as flat metric rows for a timing table.

The current engine is measured through its public first-open mode: one whole Get and
one metadata Find issued as attachment begins, then Ready, then the twelve warm
operations, and the startup work counters. The legacy engine records Ready, its first
validated query and the same warm operations. Every answer is checked before it counts.
"""

import argparse
import json
import math
import os
import platform
import shutil
import subprocess
import sys
import time
from pathlib import Path
from statistics import median

sys.path.insert(0, str(Path(__file__).resolve().parent / "live_mode"))

from baseline import READS, WRITES, generate_fixture, probe_state, verified_call, write_json
from fixture import COMMON, tree_digest
from provenance import sha256, verify_build
from results import validate_read
from transport import Current, Legacy

RECORD_VERSION = 1
LOADING = "operator-prefix-public"
# Startup counters that follow the work alone. Watcher polls and checkpoint retries
# follow timing, so a change in them is not a change in work.
WORK = ("documents_derived", "document_opens", "changesets_applied", "links_judged",
        "findings_written", "findings_discarded")
SCENARIOS = [name for name, _ in READS] + WRITES


def acknowledge(root):
    """Release each first read's caller as soon as its answer arrives.

    Validation waits until Ready, so the parent never delays the next first read.
    """
    acks = root / "operator-prefix-acks"
    acks.mkdir(parents=True, exist_ok=True)

    def handler(stage):
        sequence = stage["operator_prefix_sequence"]
        pending = acks / f"{sequence}.pending"
        write_json(pending, {"sequence": sequence, "probe": stage["probe"]})
        pending.replace(pending.with_suffix(".json"))

    return handler


def measured(name, timings, *, warmups, write_payoff):
    """Drop warmups, and for writes the one-time post-build page-split window."""
    start = max(warmups, write_payoff) if name in WRITES else warmups
    kept = timings[start:]
    if not kept:
        raise ValueError(f"{name} has no measured call after its warmups")
    return kept


def p95(values):
    ordered = sorted(values)
    return ordered[math.ceil(0.95 * len(ordered)) - 1]


def milliseconds(nanoseconds):
    return round(nanoseconds / 1e6, 3)


def row(documents, metric, value, unit, status="measured", note=""):
    return {"documents": documents, "metric": metric, "value": value, "unit": unit,
            "status": status, "note": note}


def summarize(processes):
    """Fold fresh processes into one row per metric and size.

    Clocks take the median across processes. A warm operation reports the median of
    its per-process medians and of its per-process p95s. A counter must agree across
    processes; disagreement marks it unstable.
    """
    rows = []
    for documents in sorted({process["documents"] for process in processes}):
        group = [process for process in processes if process["documents"] == documents]
        for metric in group[0]["cold"]:
            values = [process["cold"][metric] for process in group]
            refused = sum(value is None for value in values)
            if refused:
                rows.append(row(documents, metric, None, "ms", "unavailable",
                                f"{refused} of {len(values)} processes refused the first read"))
            else:
                rows.append(row(documents, metric, milliseconds(median(values)), "ms"))
        for name in group[0]["warm"]:
            samples = [process["warm"][name] for process in group]
            rows.append(row(documents, f"warm.{name}.median",
                            milliseconds(median(median(values) for values in samples)), "ms"))
            rows.append(row(documents, f"warm.{name}.p95",
                            milliseconds(median(p95(values) for values in samples)), "ms"))
        for counter in group[0]["work"]:
            values = [process["work"][counter] for process in group]
            unit = "bytes" if counter.endswith("bytes_written") else "count"
            if len(set(values)) == 1:
                rows.append(row(documents, f"work.{counter}", values[0], unit))
            else:
                rows.append(row(documents, f"work.{counter}", median(values), unit, "unstable",
                                "processes disagree: " + ", ".join(map(str, values))))
    return rows


def warm(worker, engine, root, manifest, args):
    """Time every warm operation, checking each answer outside its clock."""
    state = probe_state(manifest)
    timings = {}
    for name, base_command in READS + [(name, None) for name in WRITES]:
        calls = [verified_call(worker, engine, root, manifest, state, name, base_command, iteration)["elapsed_ns"]
                 for iteration in range(args.warmups + args.samples)]
        timings[name] = measured(name, calls, warmups=args.warmups, write_payoff=args.write_payoff)
    return timings


def bytes_written(worker, root):
    """Bytes the worker wrote through write calls, less its own measurement output."""
    io = (Path("/proc") / str(worker.process.pid) / "io").read_text().splitlines()
    written = int(next(line for line in io if line.startswith("wchar:")).split()[1])
    return written - sum((root / name).stat().st_size for name in ("worker.stdout.log", "worker.stderr.log"))


def first_reads(stages, ready, manifest):
    """Validate both public first reads; a refused one has no first-read clock."""
    from public_mode import public_answer, validate_ready
    validate_ready(stages, ready, manifest)
    clocks = {}
    for index, (metric, stage) in enumerate(zip(("first_get", "first_find"), stages)):
        answered = public_answer(index, stage, manifest) is not None
        clocks[metric] = stage["query_finished_ns"] if answered else None
    return clocks


def measure_current(binary, root, manifest, args):
    handler = acknowledge(root)
    worker = Current(binary, root, loading=LOADING, stage_handler=handler)
    try:
        # The worker is quiet once it reports Ready: its startup account is complete.
        written = bytes_written(worker, root)
        cold = first_reads(worker.stages, worker.ready, manifest)
        cold["ready"] = worker.ready["observed_ready_ns"]
        evidence = worker.ready["job_evidence"]
        work = {counter: evidence[counter] for counter in WORK}
        work["bytes_written"] = written
        timings = warm(worker, "current", root, manifest, args)
    finally:
        worker.close()
    return {"documents": manifest["document_count"], "cold": cold, "warm": timings, "work": work}


def measure_legacy(binary, root, manifest, args):
    began = time.perf_counter_ns()
    worker = Legacy(binary, root)
    try:
        first, _ = worker.call({"op": "search", "query": COMMON})
        validate_read("legacy", {"op": "search", "query": COMMON}, first, manifest)
        cold = {"ready": worker.ready["attach_ns"], "first_query": time.perf_counter_ns() - began}
        timings = warm(worker, "legacy", root, manifest, args)
    finally:
        worker.close()
    return {"documents": manifest["document_count"], "cold": cold, "warm": timings, "work": {}}


def fixture(binary, fixtures, size):
    """Read a frozen fixture, generating it once when it does not exist yet."""
    source = fixtures / str(size)
    if not source.exists():
        generate_fixture(binary, source, size)
    manifest = json.loads((source / "manifest.json").read_text())
    if manifest["workload_version"] != "runtime-baseline-v1" or manifest["background_count"] != size:
        raise ValueError("the frozen fixture belongs to another workload or scale")
    if tree_digest(source / "vault") != manifest["tree_sha256"]:
        raise ValueError("the frozen fixture no longer matches its manifest")
    return source, manifest


def identity(args):
    """The executable under measurement, frozen into the run before it starts."""
    if args.engine == "current":
        build = verify_build(args.current_build)
        if "induced-failure" not in build["features"]:
            raise ValueError("the record mode needs a provenance.py --startup-profile build")
        source = Path(build["executable"])
        compiler = next(line.split(":", 1)[1].strip() for line in build["rustc"].splitlines()
                        if line.startswith("release:"))
        commit = build["repository_head"] + ("-dirty" if build["repository_status"] else "")
        expected = build["sha256"]
    else:
        source = args.legacy.resolve(strict=True)
        expected = sha256(source)
        commit = subprocess.check_output([str(source), "--version"], text=True, timeout=30).strip()
        compiler = "unknown"
    frozen = args.output / "binaries" / args.engine
    frozen.parent.mkdir()
    shutil.copy2(source, frozen)
    if sha256(frozen) != expected:
        raise ValueError("the executable differs from its build receipt or changed while being frozen")
    return frozen, {"commit": commit, "build_sha256": expected, "compiler": compiler}


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--engine", choices=["current", "legacy"], default="current")
    parser.add_argument("--current-build", type=Path, help="provenance.py --startup-profile receipt")
    parser.add_argument("--legacy", type=Path, help="installed legacy executable")
    parser.add_argument("--fixtures", type=Path, required=True,
                        help="frozen fixture directory; a current run generates a missing size once")
    parser.add_argument("--output", type=Path, required=True, help="new, short absolute directory")
    parser.add_argument("--sizes", type=int, nargs="+", default=[2000, 10000])
    parser.add_argument("--processes", type=int, default=3, help="fresh processes per size")
    parser.add_argument("--samples", type=int, default=400)
    parser.add_argument("--warmups", type=int, default=5)
    parser.add_argument("--write-payoff", type=int, default=200,
                        help="leading write calls excluded as the one-time post-build page-split window")
    args = parser.parse_args()
    if args.engine == "current" and args.current_build is None:
        parser.error("--current-build is required for the current engine")
    if args.engine == "legacy" and args.legacy is None:
        parser.error("--legacy is required for the legacy engine")
    if platform.system() != "Linux":
        parser.error("the record mode reads Linux process accounting")
    if min(args.sizes) < 300 or args.processes < 1 or args.samples < 1 or args.warmups < 0 or args.write_payoff < 0:
        parser.error("sizes must be >=300; processes and samples positive; warmups and payoff nonnegative")
    if args.warmups + args.samples <= args.write_payoff:
        parser.error("warmups plus samples must exceed the write payoff window")
    args.output = args.output.expanduser().resolve()
    args.fixtures = args.fixtures.expanduser().resolve()
    if args.output.exists():
        parser.error("output already exists; choose a new directory")
    args.output.mkdir(parents=True)
    load_start = os.getloadavg()[0]
    metadata = {"record_version": RECORD_VERSION, "engine": args.engine, "completed": False,
                "run_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "load_start": load_start,
                "sizes": args.sizes, "processes": args.processes, "samples": args.samples,
                "warmups": args.warmups, "write_payoff": args.write_payoff}
    try:
        binary, built = identity(args)
        metadata.update(built)
        processes = []
        for size in args.sizes:
            if args.engine == "legacy" and not (args.fixtures / str(size)).exists():
                raise ValueError("only a current run generates fixtures; record the current engine first")
            source, manifest = fixture(binary, args.fixtures, size)
            for number in range(args.processes):
                root = args.output / "work" / f"{size}-{number}"
                root.mkdir(parents=True)
                shutil.copytree(source / "vault", root / "vault")
                if tree_digest(root / "vault") != manifest["tree_sha256"]:
                    raise ValueError("copied fixture differs from its frozen source")
                measure = measure_current if args.engine == "current" else measure_legacy
                processes.append(measure(binary, root, manifest, args))
                print(f"{args.engine} {manifest['document_count']} documents, process {number}: verified", flush=True)
                # Keep logs; the frozen fixture recreates every vault and store.
                for name in ("vault", "data", "cache", "state", "config"):
                    if (root / name).exists():
                        shutil.rmtree(root / name)
        metadata["rows"] = summarize(processes)
        metadata["completed"] = True
    except BaseException as problem:
        metadata["failure"] = repr(problem)
        raise
    finally:
        metadata["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        metadata["load_end"] = os.getloadavg()[0]
        write_json(args.output / "record.json", metadata)
    print(args.output / "record.json")


if __name__ == "__main__":
    main()
