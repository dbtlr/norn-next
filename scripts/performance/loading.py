#!/usr/bin/env python3
"""Compare ordinary loading with real query checkpoints in a disposable staged import."""

import argparse
import json
import os
import shutil
import sys
import time
from pathlib import Path

from baseline import READS, append, write_json
from fixture import tree_digest
from provenance import sha256, source_tree_digest, verify_build
from results import validate_read
from startup import storage_bytes
from startup_audit import audit_store
from transport import Current, resources


EXPECTED = {("catalogue", "names"), ("get", "slug"),
            ("frontmatter", "active_page"), ("frontmatter", "document_count"),
            ("frontmatter", "active_count"), ("lexical", "common"),
            ("lexical", "rare"), ("lexical", "missing"),
            ("lexical", "filtered"), ("lexical", "coverage"),
            ("rich", "section"), ("health", "summary")}


def validate_stage(stage, manifest):
    """Check each capability against authored fixture answers, including complete coverage."""
    key = stage["startup_stage"], stage["probe"]
    if key not in EXPECTED or stage["global_ready"] is not False:
        raise ValueError("unexpected or incorrectly published startup checkpoint")
    if key == ("catalogue", "names"):
        answers = {row["target"]: row for row in stage["candidates"]}
        if answers["small"]["paths"] != ["bench/small.md"] or answers["small"]["total"] != 1:
            raise ValueError("catalogue did not resolve the unique slug")
        if answers["roadmap"]["total"] != 6 or len(answers["roadmap"]["paths"]) != 5:
            raise ValueError("catalogue did not preserve the complete ambiguity class")
        if answers["profilenonexistentdocument"]["total"] != 0:
            raise ValueError("catalogue invented a missing name")
        return
    command = stage["command"]
    if command["op"] in ("get", "find", "search"):
        expected_command = command
        if key == ("get", "slug"):
            if command["target"] != "small":
                raise ValueError("the first get did not use a slug")
            expected_command = {"op": "get", "target": "bench/small.md"}
        validate_read("current", expected_command, stage, manifest)
    elif command["op"] == "count":
        answer = stage["answer"]
        expected = len(manifest["active_paths"]) if command.get("active") else manifest["document_count"]
        report = answer["report"]
        if answer["unsatisfied"] or answer["advisories"] or report["next"] is not None:
            raise ValueError("coverage count was not fully answered")
        if len(report["rows"]) != 1 or report["rows"][0]["count"] != expected:
            raise ValueError("a checkpoint became available before its whole corpus was loaded")
    elif command["op"] == "validate":
        if stage["answer"]["unsatisfied"] or stage["answer"]["advisories"]:
            raise ValueError("final validation was not fully answered")
        if stage["answer"]["report"]["shape"] != "summary":
            raise ValueError("final health did not answer a summary")


def run_one(binary, root, manifest, mode):
    """Keep query validation in readiness clocks and persisted-state audit outside them."""
    began = time.perf_counter_ns()
    keys = []

    def on_stage(stage):
        validate_stage(stage, manifest)
        key = stage["startup_stage"], stage["probe"]
        if key in keys:
            raise ValueError("duplicate checkpoint")
        keys.append(key)
        stage["validated_after_launch_ns"] = time.perf_counter_ns() - began
        stage["verified"] = True

    worker = Current(binary, root, profiling=True,
                     loading=None if mode == "baseline" else mode, stage_handler=on_stage)
    try:
        ready_observed = time.perf_counter_ns() - began
        checked = []
        for name, command in READS:
            answer, elapsed = worker.call(command)
            validate_read("current", command, answer, manifest)
            checked.append({"name": name, "round_trip_ns": elapsed, "host_ns": answer["host_ns"]})
        if mode == "baseline" and keys or mode != "baseline" and set(keys) != EXPECTED:
            raise ValueError("startup checkpoint coverage differs from the requested mode")
        result = {"mode": mode, "ready_observed_ns": ready_observed, "ready": worker.ready,
                  "stages": worker.stages, "checks": checked, "load_at_ready": os.getloadavg(),
                  "resources_at_ready": resources(worker.process.pid),
                  "storage_at_ready": storage_bytes(root)}
    finally:
        closing = time.perf_counter_ns()
        worker.close()
        closed = time.perf_counter_ns()
    result["shutdown_ns"] = closed - closing
    result["validated_lifecycle_ns"] = closed - began
    result["store"] = audit_store(binary, root, manifest)
    if mode != "baseline":
        summary = next(s for s in result["stages"] if s["startup_stage"] == "health")
        total = sum(row["count"] for row in summary["answer"]["report"]["by_kind"])
        if total != result["store"]["counts"]["findings"]:
            raise ValueError("the health checkpoint preceded its complete findings")
    result["verified"] = True
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sizes", nargs="+", type=int, default=[300, 2000, 10000])
    parser.add_argument("--modes", nargs="+", choices=["baseline", "layered"], default=["baseline", "layered"])
    parser.add_argument("--repetitions", type=int, default=2)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    receipt = verify_build(args.build, repo)
    if "induced-failure" not in receipt["features"]:
        parser.error("loading experiments require the explicitly instrumented build")
    if args.output.exists() or args.output.resolve().is_relative_to(repo) or args.repetitions < 1:
        parser.error("choose a new output directory outside the repository and positive repetitions")
    args.output.mkdir(parents=True)
    binary = (args.output / "worker").resolve()
    shutil.copy2(receipt["executable"], binary)
    assert sha256(binary) == receipt["sha256"]
    runner = args.output / "runner"
    runner.mkdir()
    for path in Path(__file__).parent.glob("*.py"):
        shutil.copy2(path, runner / path.name)
    metadata = {"completed": False, "build": receipt, "source_sha256": source_tree_digest(repo),
                "sizes": args.sizes, "modes": args.modes, "repetitions": args.repetitions,
                "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "load_start": os.getloadavg()}
    write_json(args.output / "run.json", metadata)
    references = {}
    try:
        fixtures = {}
        for size in args.sizes:
            source = args.fixtures / str(size)
            manifest = json.loads((source / "manifest.json").read_text())
            if tree_digest(source / "vault") != manifest["tree_sha256"]:
                raise ValueError("fixture identity changed")
            frozen = args.output / "fixtures" / str(size)
            frozen.mkdir(parents=True)
            shutil.copytree(source / "vault", frozen / "vault")
            write_json(frozen / "manifest.json", manifest)
            fixtures[size] = frozen, manifest
        with (args.output / "samples.jsonl").open("w") as samples:
            for repetition in range(args.repetitions):
                modes = args.modes if repetition % 2 == 0 else list(reversed(args.modes))
                sizes = args.sizes[repetition % len(args.sizes):] + args.sizes[:repetition % len(args.sizes)]
                for size in sizes:
                    frozen, manifest = fixtures[size]
                    for mode in modes:
                        root = (args.output / "work" / f"r{repetition}-n{size}-{mode}").resolve()
                        root.mkdir(parents=True)
                        shutil.copytree(frozen / "vault", root / "vault")
                        if tree_digest(root / "vault") != manifest["tree_sha256"]:
                            raise ValueError("copied fixture changed")
                        result = run_one(binary, root, manifest, mode)
                        digest = result["store"]["semantic_sha256"]
                        if references.setdefault(size, digest) != digest:
                            raise ValueError("staging changed the final index")
                        append(samples, {"repetition": repetition, "background_documents": size,
                                         "document_count": manifest["document_count"], **result})
                        milestones = {s["startup_stage"]: round(s["validated_after_launch_ns"] / 1e9, 4)
                                      for s in result["stages"]}
                        print(size, mode, round(result["ready_observed_ns"] / 1e9, 3), milestones, "verified", flush=True)
        if source_tree_digest(repo) != metadata["source_sha256"]:
            raise ValueError("source changed during measurement")
        metadata["completed"] = True
    except BaseException as error:
        metadata["failure"] = repr(error)
        raise
    finally:
        metadata["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        metadata["load_end"] = os.getloadavg()
        write_json(args.output / "run.json", metadata)


if __name__ == "__main__":
    main()
