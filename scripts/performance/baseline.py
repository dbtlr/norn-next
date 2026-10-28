#!/usr/bin/env python3
"""Run the frozen search/get/write workload and preserve every observation."""

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import struct
from statistics import median
import time
from pathlib import Path

from fixture import COMMON, MISSING, RARE, prepare_probes, tree_digest
from results import record, validate_read, validate_write
from provenance import sha256, verify_build
from transport import Current, Legacy, environment, issue_root, resources


READS = [
    ("search_rare", {"op": "search", "query": RARE}),
    ("search_common", {"op": "search", "query": COMMON}),
    ("search_missing", {"op": "search", "query": MISSING}),
    ("search_filtered", {"op": "search", "query": COMMON, "path": "bench/**"}),
    ("find_metadata", {"op": "find"}),
    ("get_small", {"op": "get", "target": "bench/small.md"}),
    ("get_large", {"op": "get", "target": "bench/large.md"}),
    ("get_section", {"op": "get", "target": "bench/large.md#Details"}),
]
WRITES = ["set_field", "create", "rename", "rename_hub"]


def wal_position(root):
    """Read committed frames from SQLite's WAL index, whose length survives resets.

    The worker is idle at both endpoints. The salt distinguishes WAL epochs;
    retained WAL file length cannot measure writes after an autocheckpoint.
    """
    stores = list((root / "data").rglob("store.sqlite3"))
    if len(stores) != 1:
        raise ValueError("WAL readings require exactly one store.sqlite3")
    db = stores[0]
    wal, shm = Path(str(db) + "-wal"), Path(str(db) + "-shm")
    if not wal.exists() or wal.stat().st_size == 0:
        return None
    with wal.open("rb") as file:
        header = file.read(32)
    with shm.open("rb") as file:
        index = file.read(48)
    if len(header) != 32 or len(index) != 48:
        raise ValueError("incomplete WAL header or WAL index")
    return header[16:24].hex(), int.from_bytes(header[8:12], "big"), struct.unpack_from("=I", index, 16)[0]


def wal_pages_written(before, after):
    """Count appended frames; an epoch reset makes the observation incomplete."""
    if before is None:
        return after[2] if after is not None else 0
    if after is None or before[:2] != after[:2] or after[2] < before[2]:
        return None
    return after[2] - before[2]


def write_phase_summary(samples):
    """Keep cold-write debt separate from the writes after its first 200 operations."""
    result = {}
    for phase, selected in [("first_200", [s for s in samples if s["iteration"] < 200]),
                            ("after_payoff", [s for s in samples if s["iteration"] >= 200])]:
        pages = [s["wal_pages_per_write"] for s in selected if s["wal_pages_per_write"] is not None]
        result[phase] = {"writes": len(selected),
                         "median_ns": median([s["round_trip_ns"] for s in selected]) if selected else None,
                         "median_wal_pages_per_write": median(pages) if pages else None,
                         "wal_observations": len(pages)}
    return result


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def append(file, value):
    file.write(json.dumps(value, separators=(",", ":")) + "\n")
    file.flush()


def mutation(name, iteration):
    if name == "set_field":
        return {"op": "set", "value": f"value{iteration}"}
    if name == "create":
        body = f"# Created {iteration}\nA new document with known bytes.\n"
        return {"op": "new", "path": f"bench/created-{iteration:04}.md", "body": body,
                "content": "---\nstatus: draft\n---\n" + body}
    stem = "subject" if name == "rename" else "hub"
    first, second = f"bench/{stem}.md", f"bench/{stem}-moved.md"
    source, destination = (first, second) if iteration % 2 == 0 else (second, first)
    return {"op": "move", "from": source, "to": destination}


def verify_mutation(worker, engine, command, root, response, state, before_bytes=None):
    validate_write(engine, response)
    target = {"set": "bench/subject.md", "new": command.get("path"),
              "move": command.get("to")}[command["op"]]
    readback, read_ns = worker.call({"op": "get", "target": target})
    document = record(engine, readback)
    if command["op"] == "set":
        expected = {**state[target], "fields": {**state[target]["fields"], "status": command["value"]}}
    elif command["op"] == "new":
        expected = {"path": target, "body": command["body"], "fields": {"status": "draft"}}
        if (root / "vault" / target).read_bytes() != command["content"].encode():
            raise ValueError("created bytes differ from the authored document")
    else:
        expected = {**state[command["from"]], "path": target}
        if (root / "vault" / command["from"]).exists():
            raise ValueError("rename left the source behind")
        if "hub" in target:
            link = "[[" + target.removesuffix(".md") + "]]"
            previous_link = "[[" + command["from"].removesuffix(".md") + "]]"
            for number in range(10):
                name = f"bench/referrer-{number:02}.md"
                body = state[name]["body"].replace(previous_link, link)
                data = (root / "vault" / name).read_bytes()
                if data != ("---\nstatus: draft\nperf_bucket: probe\n---\n" + body).encode():
                    raise ValueError("rename did not preserve a referrer's content around its rewritten link")
                state[name] = {**state[name], "body": body}
        del state[command["from"]]
    if document != expected:
        raise ValueError("the write lost or changed content outside its authored effect")
    if command["op"] in ("set", "move"):
        if before_bytes is None:
            raise ValueError("a mutation needs its independently captured before-bytes")
        after_bytes = (root / "vault" / target).read_bytes()
        if command["op"] == "move" and after_bytes != before_bytes:
            raise ValueError("rename changed the destination's bytes")
        if command["op"] == "set":
            before_lines, after_lines = before_bytes.splitlines(keepends=True), after_bytes.splitlines(keepends=True)
            field = next(index for index, line in enumerate(before_lines) if line.startswith(b"status:"))
            allowed = [f"status: {value}\n".encode() for value in
                       (command["value"], json.dumps(command["value"]), "'" + command["value"] + "'")]
            if len(before_lines) != len(after_lines) or after_lines[field] not in allowed:
                raise ValueError("the field value was not written to disk")
            if before_lines[:field] + before_lines[field + 1:] != after_lines[:field] + after_lines[field + 1:]:
                raise ValueError("set changed bytes outside the authored field")
    state[target] = expected
    diagnostics = readback.get("answer", {}).get("advisories", []) if engine == "current" else readback["structuredContent"].get("notes", [])
    return read_ns, diagnostics


def run_worker(args, engine, size, repetition, fixture, manifest, samples):
    short = "c" if engine == "current" else "l"
    root = args.output / "work" / f"{size}-{repetition}-{short}"
    root.mkdir(parents=True)
    shutil.copytree(fixture / "vault", root / "vault")
    if tree_digest(root / "vault") != manifest["tree_sha256"]:
        raise ValueError("copied fixture differs from its frozen source")
    cls, binary = (Current, args.current) if engine == "current" else (Legacy, args.legacy)
    context = {"engine": engine, "background_documents": size, "repetition": repetition,
               "fixture_sha256": manifest["tree_sha256"], "document_count": manifest["document_count"]}
    state = {name: {"path": name, "body": body, "fields": {"status": "draft", "perf_bucket": "probe"}}
             for name, body in manifest["probe_bodies"].items()}
    began = time.perf_counter_ns()
    loading = getattr(args, "loading", None)
    if loading and engine != "current":
        raise ValueError("public first-open loading is only supported by the current worker")
    worker = cls(binary, root, loading=loading) if loading else cls(binary, root)
    try:
        if loading:
            if (loading != "operator-prefix-public" or len(worker.stages) != 2
                    or {(row["startup_stage"], row["probe"]) for row in worker.stages}
                    != {("public-get", "whole_slug"), ("public-find", "active_page")}):
                raise ValueError("warm comparison did not execute the requested loading mode")
            write_json(root / "loading-checkpoints.json", worker.stages)
        # A control hello can precede lazy indexing. Readiness includes a real validated query.
        first, _ = worker.call({"op": "search", "query": COMMON})
        validate_read(engine, {"op": "search", "query": COMMON}, first, manifest)
        startup = time.perf_counter_ns() - began
        append(samples, {**context, "kind": "startup", "first_validated_query_ns": startup,
                         "ready": worker.ready, "identity": getattr(worker, "identity", None),
                         "resources": resources(worker.process.pid)})
        write_json(root / "first-query.json", first)
        print(f"{engine} {size} round {repetition}: attached", flush=True)

        for name, base_command in READS + [(name, None) for name in WRITES]:
            before = resources(worker.process.pid)
            writes = []
            for iteration in range(args.warmups + args.samples):
                command = base_command or mutation(name, iteration)
                before_path = "bench/subject.md" if command["op"] == "set" else command.get("from")
                before_bytes = (root / "vault" / before_path).read_bytes() if before_path is not None else None
                wal_before = wal_position(root) if base_command is None and engine == "current" else None
                response, elapsed = worker.call(command)
                wal_after = wal_position(root) if base_command is None and engine == "current" else None
                if base_command is not None:
                    validate_read(engine, command, response, manifest)
                    readback = None
                    readback_diagnostics = None
                else:
                    readback, readback_diagnostics = verify_mutation(worker, engine, command, root, response, state, before_bytes)
                if base_command is None:
                    writes.append({"iteration": iteration, "round_trip_ns": elapsed,
                                   "wal_pages_per_write": wal_pages_written(wal_before, wal_after) if engine == "current" else None})
                if iteration == args.warmups:
                    write_json(root / f"{name}-example.json", {"command": command, "response": response})
                append(samples, {**context, "kind": "sample", "scenario": name,
                                 "write_phase": ("first_200" if iteration < 200 else "after_payoff") if base_command is None else None,
                                 "wal_pages_per_write": wal_pages_written(wal_before, wal_after) if base_command is None and engine == "current" else None,
                                 "wal_epoch_reset": (wal_before is not None and wal_pages_written(wal_before, wal_after) is None) if base_command is None and engine == "current" else None,
                                 "iteration": iteration, "warmup": iteration < args.warmups,
                                 "command": command, "round_trip_ns": elapsed,
                                 "host_ns": response.get("host_ns"),
                                 "advisories": response.get("answer", {}).get("advisories", []),
                                 "legacy_warnings": response.get("structuredContent", {}).get("report", {}).get("warnings", []),
                                 "counters": response.get("counters"),
                                 "readback_ns": readback,
                                 "readback_diagnostics": readback_diagnostics,
                                 "write_and_readback_ns": elapsed + readback if readback is not None else None,
                                 "verified": True})
            if writes:
                append(samples, {**context, "kind": "write_phases", "scenario": name,
                                 "phases": write_phase_summary(writes)})
            append(samples, {**context, "kind": "resources", "scenario": name,
                             "before": before, "after": resources(worker.process.pid)})
            print(f"{engine} {size} round {repetition}: {name} verified", flush=True)
        time.sleep(1)
        append(samples, {**context, "kind": "end", "resources": resources(worker.process.pid),
                         "final_tree_sha256": tree_digest(root / "vault")})
    finally:
        worker.close()


def prepare(args, size):
    root = args.output / "fixtures" / str(size)
    if args.fixtures is not None:
        source = args.fixtures / str(size)
        manifest = json.loads((source / "manifest.json").read_text())
        if manifest["workload_version"] != "runtime-baseline-v1" or manifest["background_count"] != size:
            raise ValueError("the frozen fixture belongs to another workload or scale")
        required = {"probe_bodies", "paths", "rare_paths", "active_paths", "filtered_paths", "small_body",
                    "large_body", "section", "tree_sha256", "document_count"}
        if not required <= manifest.keys():
            raise ValueError("the frozen fixture is missing required known-answer data")
        if tree_digest(source / "vault") != manifest["tree_sha256"]:
            raise ValueError("the frozen fixture no longer matches its manifest")
        root.mkdir(parents=True)
        shutil.copytree(source / "vault", root / "vault")
        write_json(root / "manifest.json", manifest)
        return root, manifest
    token = issue_root(root)
    env = environment(root, token)
    env["NORN_PERFORMANCE_DOCUMENTS"] = str(size)
    with (root / "generation.log").open("w") as log:
        subprocess.run([str(args.current), "generate"],
                       env=env, cwd=root, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=300)
    manifest = prepare_probes(root / "vault")
    write_json(root / "manifest.json", manifest)
    return root, manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current-build", type=Path, required=True, help="verified worker build receipt")
    parser.add_argument("--legacy", type=Path, required=True, help="legacy executable")
    parser.add_argument("--output", type=Path, required=True, help="new, short absolute directory outside the repository")
    parser.add_argument("--fixtures", type=Path, help="previous run's frozen fixtures directory")
    parser.add_argument("--sizes", type=int, nargs="+", default=[300, 2000, 10000])
    parser.add_argument("--samples", type=int, default=25, help="use at least 400 for first-200 and after-payoff write comparison")
    parser.add_argument("--warmups", type=int, default=5)
    parser.add_argument("--repetitions", type=int, default=2)
    args = parser.parse_args()
    args.output = args.output.expanduser().resolve()
    build = verify_build(args.current_build)
    args.current, args.legacy = Path(build["executable"]).resolve(strict=True), args.legacy.resolve(strict=True)
    if platform.system() != "Linux":
        parser.error("this initial process-resource collector requires Linux")
    if min(args.sizes) < 300 or args.samples < 1 or args.warmups < 0 or args.repetitions < 1:
        parser.error("sizes must be >=300; samples and repetitions positive; warmups nonnegative")
    if args.output.exists():
        parser.error("output already exists; preserve previous runs and choose a new directory")
    args.output.mkdir(parents=True)
    binaries = args.output / "binaries"
    binaries.mkdir()
    for name in ("current", "legacy"):
        source = getattr(args, name)
        expected_hash = sha256(source)
        target = binaries / name
        shutil.copy2(source, target)
        if sha256(target) != expected_hash:
            raise ValueError("an executable changed while its run copy was being frozen")
        setattr(args, name, target)
    if sha256(args.current) != build["sha256"]:
        raise ValueError("the frozen current worker differs from its build receipt")
    repo = Path(__file__).resolve().parents[2]
    metadata = {
        "workload_version": "runtime-baseline-v1", "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "current_sha256": sha256(args.current), "legacy_sha256": sha256(args.legacy),
        "current_build": build,
        "current_executable": str(args.current), "legacy_executable": str(args.legacy),
        "legacy_version": subprocess.check_output([str(args.legacy), "--version"], text=True).strip(),
        "repository_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
        "repository_status": subprocess.check_output(["git", "status", "--porcelain"], cwd=repo, text=True),
        "harness_sha256": {str(path.relative_to(repo)): sha256(path) for path in
                           [*sorted(Path(__file__).parent.glob("*.py")), repo / "crates/norn-host/tests/performance.rs"]},
        "platform": platform.platform(), "python": platform.python_version(),
        "filesystem": subprocess.check_output(["stat", "-f", "-c", "%T", str(args.output)], text=True).strip(),
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "cpu": next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                     if line.startswith("model name")), "unknown"),
        "load_start": os.getloadavg(), "samples": args.samples, "warmups": args.warmups,
        "repetitions": args.repetitions, "sizes": args.sizes,
        "cache_conditions": "fresh derived stores; OS filesystem caches uncontrolled; warm requests after explicit warmups",
        "legacy_build_profile": "installed binary; build flags unknown",
        "fixture_source": str(args.fixtures.resolve()) if args.fixtures else "generated for this run",
        "scope": "current production host through JSONL test adapter; legacy warm daemon through Unix-socket MCP",
        "completed": False,
    }
    write_json(args.output / "run.json", metadata)
    try:
        # Generate all fixtures before any timings, then run engines serially in alternating order.
        fixtures = {size: prepare(args, size) for size in args.sizes}
        with (args.output / "samples.jsonl").open("w") as samples:
            for size, (fixture, manifest) in fixtures.items():
                for repetition in range(args.repetitions):
                    order = ("current", "legacy") if repetition % 2 == 0 else ("legacy", "current")
                    for engine in order:
                        run_worker(args, engine, size, repetition, fixture, manifest, samples)
        metadata["completed"] = True
    except BaseException as problem:
        metadata["failure"] = repr(problem)
        raise
    finally:
        metadata["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        metadata["load_end"] = os.getloadavg()
        write_json(args.output / "run.json", metadata)


if __name__ == "__main__":
    main()
