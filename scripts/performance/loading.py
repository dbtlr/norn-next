#!/usr/bin/env python3
"""Compare ordinary loading with real query checkpoints in a disposable staged import."""

import argparse
import base64
import copy
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

from baseline import READS, append, write_json
from fixture import tree_digest
from provenance import sha256, source_tree_digest, verify_build
from results import validate_read
from startup import storage_bytes, validate_work
from startup_audit import audit_store
from transport import Current, PREFIX, environment, resources


EXPECTED = {("catalogue", "names"), ("get", "slug"),
            ("frontmatter", "active_page"), ("frontmatter", "document_count"),
            ("frontmatter", "active_count"), ("lexical", "common"),
            ("lexical", "rare"), ("lexical", "missing"),
            ("lexical", "filtered"), ("lexical", "coverage"),
            ("rich", "section"), ("health", "summary")}
SILENT_EXPECTED = {(stage, "boundary") for stage in
                   ("catalogue", "get", "frontmatter", "lexical", "rich", "health")}

CANONICAL_EXPECTED = (EXPECTED - {("get", "slug")}) | {
    ("canonical-demand", "whole_slug"), ("canonical-health", "whole_slug")}
CANONICAL_SILENT_EXPECTED = {(stage, "boundary") for stage in
                             ("catalogue", "canonical-demand", "canonical-health")}


INTAKE_PROBES = {(stage, probe) for stage in ("canonical-intake", "canonical-final-intake") for probe in
                 ("active_page", "document_count", "active_count", "sparse_page", "zero_page", "unknown_field", "field_page")}


def expected_checkpoints(mode):
    """One protocol definition shared by cold and warm controllers."""
    if mode in ("baseline", "operator-prefix-ordinary"):
        return set()
    if mode == "operator-prefix":
        from operator_prefix import STAGES
        return set(STAGES)
    if mode in ("canonical-intake", "canonical-intake-silent"):
        return (CANONICAL_SILENT_EXPECTED | {("canonical-intake", "boundary")}
                if mode.endswith("-silent") else CANONICAL_EXPECTED | INTAKE_PROBES)
    if mode in ("canonical-demand", "canonical-demand-silent"):
        return CANONICAL_SILENT_EXPECTED if mode.endswith("-silent") else CANONICAL_EXPECTED
    if mode in ("layered", "layered-silent", "facts-once", "facts-once-silent"):
        return SILENT_EXPECTED if mode.endswith("-silent") else EXPECTED
    raise ValueError(f"unknown loading mode: {mode}")


def whole_get_answer(stage):
    """Keep the complete record while comparing finding content across store epochs."""
    answer = copy.deepcopy(stage["answer"])
    report = answer["report"]
    document = report.get("document", {})
    required = {"path", "body", "fields", "headings", "blocks", "tags", "links", "findings"}
    if report.get("shape") != "record" or not required <= document.keys():
        raise ValueError("whole-record comparison cannot omit requested collections")
    epoch, generation = stage.get("projection_epoch"), stage.get("projection_generation")
    if not isinstance(epoch, str) or not epoch or type(generation) is not int or generation < 0:
        raise ValueError("whole-record projection provenance is missing or invalid")
    findings = document["findings"]
    if findings["total"] < len(findings["items"]):
        raise ValueError("whole-record finding total is below its returned head")
    finding_ids = set()
    for finding in findings["items"]:
        if (type(finding.get("id")) is not int or finding["id"] <= 0
                or type(finding.get("generation")) is not int
                or not 0 <= finding["generation"] <= generation):
            raise ValueError("whole-record finding provenance is outside its projection")
        if finding["id"] in finding_ids:
            raise ValueError("whole-record finding handles contain a duplicate")
        finding_ids.add(finding["id"])
        finding.pop("id")
        finding.pop("generation")
    return answer


def metadata_answer(stage):
    """Normalize only verified projection identity; preserve query content and cursor keys."""
    answer = copy.deepcopy(stage["answer"])
    report = answer["report"]

    def check(reading):
        if (reading["epoch"] != stage["projection_epoch"]
                or reading["generation"] != stage["projection_generation"]):
            raise ValueError("metadata projection provenance disagrees with its answer")

    if "counted_at" in report:
        check(report["counted_at"])
        report["counted_at"].pop("epoch")
        report["counted_at"].pop("generation")
    page = report.get("page", report)
    if page.get("next") is not None:
        encoded = page["next"]
        cursor = json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))
        check(cursor["snapshot"])
        cursor["snapshot"].pop("epoch")
        cursor["snapshot"].pop("generation")
        page["next"] = cursor
    return answer


def validate_metadata(stage, manifest):
    if stage.get("simulation") != "paused":
        raise ValueError("metadata checkpoint must identify its paused simulation")
    answer = metadata_answer(stage)
    report = answer["report"]
    probe = stage["probe"]
    if stage["command"]["op"] == "count":
        expected = len(manifest["active_paths"]) if probe == "active_count" else manifest["document_count"]
        if (answer["unsatisfied"] or answer["advisories"] or report["next"] is not None
                or report["rows"] != [{"group": [], "count": expected}]):
            raise ValueError("metadata count differs from the complete corpus")
        return
    if probe == "unknown_field":
        parts = answer["unsatisfied"]
        if (len(parts) != 1 or parts[0].get("part") != "unknown_predicate_key"
                or parts[0].get("key") != "profilenonexistentkey"):
            raise ValueError("metadata query lost the unknown-key diagnostic")
    elif answer["unsatisfied"]:
        raise ValueError("metadata query was partially satisfied")
    expected = (manifest["active_paths"] if probe == "active_page" else
                manifest["filtered_paths"] if probe in ("sparse_page", "field_page") else [])
    fold = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")
    expected = sorted(expected, key=lambda path: (path.translate(fold), path), reverse=probe == "field_page")
    page = report["page"]
    if (answer["advisories"] or page["moved"] or [row["path"] for row in page["rows"]] != expected[:25]
            or (page["next"] is not None) != (len(expected) > 25)):
        raise ValueError("metadata page differs from the complete ordered match")
    if probe == "field_page" and any(row.get("fields", {}).get("perf_bucket", {}).get("raw") != "probe" for row in page["rows"]):
        raise ValueError("metadata field projection differs from the fixture")


def validate_stage(stage, manifest):
    """Check each capability against authored fixture answers, including complete coverage."""
    key = stage["startup_stage"], stage["probe"]
    if key not in EXPECTED | CANONICAL_EXPECTED | INTAKE_PROBES or stage["global_ready"] is not False:
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
    if key in INTAKE_PROBES:
        validate_metadata(stage, manifest)
        return
    command = stage["command"]
    if key[0] in ("canonical-demand", "canonical-health"):
        document = stage["answer"]["report"].get("document", {})
        if (command.get("whole") is not True or stage.get("simulation") != "paused"
                or any(column not in document for column in ("body", "fields", "headings", "blocks", "tags", "links", "findings"))):
            raise ValueError("canonical-demand must answer a whole-record get in a paused simulation")

    if command["op"] in ("get", "find", "search"):
        expected_command = command
        if key == ("get", "slug") or key[0] in ("canonical-demand", "canonical-health"):
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


def run_one(binary, root, manifest, mode, *, profiling=True):
    """Keep query validation in readiness clocks and persisted-state audit outside them."""
    began = time.perf_counter_ns()
    keys = []
    canonical = mode.startswith("canonical-")
    expected = expected_checkpoints(mode)
    root.mkdir(parents=True, exist_ok=True)

    def on_stage(stage):
        key = stage["startup_stage"], stage["probe"]
        if mode.endswith("-silent"):
            if key not in expected or stage.get("queries_executed") is not False:
                raise ValueError("a silent control emitted a query claim")
        else:
            validate_stage(stage, manifest)
        if key in keys:
            raise ValueError("duplicate checkpoint")
        keys.append(key)
        stage["validated_after_launch_ns"] = time.perf_counter_ns() - began
        stage["verified"] = True
        with (root / "stages.jsonl").open("a") as log:
            append(log, stage)

    worker = Current(binary, root, profiling=profiling,
                     loading=None if mode == "baseline" else mode, stage_handler=on_stage)
    try:
        ready_observed = time.perf_counter_ns() - began
        checked = []
        for name, command in READS:
            answer, elapsed = worker.call(command)
            validate_read("current", command, answer, manifest)
            checked.append({"name": name, "round_trip_ns": elapsed, "host_ns": answer["host_ns"]})
        if set(keys) != expected:
            raise ValueError("startup checkpoint coverage differs from the requested mode")
        if canonical and not mode.endswith("-silent"):
            early = next(s for s in worker.stages if s["startup_stage"] == "canonical-demand")
            final = next(s for s in worker.stages if s["startup_stage"] == "canonical-health")
            if whole_get_answer(early) != whole_get_answer(final):
                raise ValueError("demand whole-record answer differs from ordinary final answer")
        if mode == "canonical-intake":
            early_rows = {s["probe"]: s for s in worker.stages if s["startup_stage"] == "canonical-intake"}
            final_rows = {s["probe"]: s for s in worker.stages if s["startup_stage"] == "canonical-final-intake"}
            for probe, early in early_rows.items():
                if metadata_answer(early) != metadata_answer(final_rows[probe]):
                    raise ValueError(f"metadata answer differs from ordinary final answer: {probe}")
        milestones = {}
        for stage in worker.stages:
            name = stage["startup_stage"]
            milestones[name] = min(milestones.get(name, stage["stage_boundary_ns"]), stage["stage_boundary_ns"])
        result = {"mode": mode, "ready_observed_ns": ready_observed, "ready": worker.ready,
                  "checkpoint_boundaries_ns": milestones,
                  "probe_query_ns": sum(stage["query_host_ns"] for stage in worker.stages),
                  "stages": worker.stages, "checks": checked, "load_at_ready": os.getloadavg(),
                  "resources_at_ready": resources(worker.process.pid),
                  "storage_at_ready": storage_bytes(root)}
    finally:
        closing = time.perf_counter_ns()
        worker.close()
        closed = time.perf_counter_ns()
    result["shutdown_ns"] = closed - closing
    result["validated_lifecycle_ns"] = closed - began
    verification = subprocess.run([str(binary), "verify"], cwd=root,
        env=environment(root, (root / "performance-token").read_text()),
        capture_output=True, text=True, timeout=300, check=True)
    (root / "integrity.stdout.log").write_text(verification.stdout)
    (root / "integrity.stderr.log").write_text(verification.stderr)
    if verification.stdout.strip() != PREFIX + '{"integrity_verified":true}':
        raise ValueError("integrity worker did not confirm its check")
    result["integrity_verified"] = True
    result["store"] = audit_store(binary, root, manifest)
    if mode != "baseline" and not mode.endswith("-silent"):
        summary = next(s for s in result["stages"] if s["startup_stage"] == "health")
        total = sum(row["count"] for row in summary["answer"]["report"]["by_kind"])
        if total != result["store"]["counts"]["findings"]:
            raise ValueError("the health checkpoint preceded its complete findings")
    for stream in ("stdout", "stderr"):
        shutil.copy2(root / f"worker.{stream}.log", root / f"fresh.{stream}.log")
    restart = Current(binary, root, profiling=profiling, loading=None if mode == "baseline" else mode)
    try:
        if restart.stages:
            raise ValueError("unchanged restart entered the initial loading experiment")
        validate_work(restart.ready["job_evidence"], "restart", manifest["document_count"], 128)
        for _, command in READS:
            answer, _ = restart.call(command)
            validate_read("current", command, answer, manifest)
        result["restart"] = {"ready": restart.ready, "resources": resources(restart.process.pid)}
    finally:
        restart.close()
    restarted = audit_store(binary, root, manifest)
    for key in ("store_epoch", "write_generation", "semantic_sha256"):
        if restarted[key] != result["store"][key]:
            raise ValueError("unchanged restart rebuilt or changed the final index")
    result["restart"]["verified"] = True
    result["verified"] = True
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sizes", nargs="+", type=int, default=[300, 2000, 10000])
    parser.add_argument("--modes", nargs="+", choices=["baseline", "layered", "layered-silent", "facts-once", "facts-once-silent", "canonical-demand", "canonical-demand-silent", "canonical-intake", "canonical-intake-silent"], default=["baseline", "layered", "layered-silent"])
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--no-profile", action="store_true", help="disable optional profiling spans in measured workers")
    args = parser.parse_args()
    if "baseline" not in args.modes or len(set(args.modes)) != len(args.modes) or len(set(args.sizes)) != len(args.sizes):
        parser.error("include one baseline and use unique modes and sizes")
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
                "profiling": not args.no_profile,
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
                        result = run_one(binary, root, manifest, mode, profiling=not args.no_profile)
                        digest = result["store"]["semantic_sha256"]
                        if references.setdefault(size, digest) != digest:
                            raise ValueError("staging changed the final index")
                        append(samples, {"repetition": repetition, "background_documents": size,
                                         "document_count": manifest["document_count"], **result})
                        milestones = {name: round(value / 1e9, 4) for name, value in result["checkpoint_boundaries_ns"].items()}
                        print(size, mode, round(result["ready"]["startup_complete_ns"] / 1e9, 3), milestones, "verified", flush=True)
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
