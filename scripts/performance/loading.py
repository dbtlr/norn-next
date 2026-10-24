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
from results import validate_read, validate_first_get, whole_get_answer
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


def construction_group(mode, group):
    """Reject a second factor or an implicit group selection."""
    if mode == "health-once-txn":
        if group not in ("1", "16", "unbounded"):
            raise ValueError("health-once-txn requires group 1, 16, or unbounded")
    elif group is not None:
        raise ValueError("construction groups require health-once-txn")
    return group


def expected_checkpoints(mode):
    """One protocol definition shared by cold and warm controllers."""
    if mode == "baseline":
        return set()
    if mode == "health-once-txn":
        return expected_checkpoints("health-once") | {("construction-complete", "boundary")}
    if mode == "health-once":
        return {("health-once-facts", "boundary"), ("health-once-health", "boundary")}
    if mode in ("canonical-intake", "canonical-intake-silent"):
        return (CANONICAL_SILENT_EXPECTED | {("canonical-intake", "boundary")}
                if mode.endswith("-silent") else CANONICAL_EXPECTED | INTAKE_PROBES)
    if mode in ("canonical-demand", "canonical-demand-silent"):
        return CANONICAL_SILENT_EXPECTED if mode.endswith("-silent") else CANONICAL_EXPECTED
    if mode in ("layered", "layered-silent", "facts-once", "facts-once-silent"):
        return SILENT_EXPECTED if mode.endswith("-silent") else EXPECTED
    raise ValueError(f"unknown loading mode: {mode}")


DOCUMENT_WORK = ("documents_derived", "documents_upserted", "documents_deleted",
                 "tombstones_recorded", "changesets_applied")
HEALTH_WORK = ("links_redecided", "link_health_keys_resolved", "link_health_candidates_read")
WRITER_WORK = DOCUMENT_WORK + HEALTH_WORK + ("findings_written", "findings_discarded", "changeset_read_steps")
ORDINARY_WORK = ("document_opens", "stats", "walk_dirents") + DOCUMENT_WORK


def nonnegative(value, name):
    if type(value) is not int or value < 0:
        raise ValueError(f"{name} must be a nonnegative integer")
    return value


def validate_fresh_work(ready, manifest):
    evidence = ready.get("job_evidence")
    if not isinstance(evidence, dict):
        raise ValueError("fresh startup requires its Ready work account")
    for key, value in evidence.items():
        nonnegative(value, key)
    for key in ORDINARY_WORK + HEALTH_WORK + ("findings_written", "findings_discarded",
                                             "changeset_read_steps", "recoveries_run", "rebuilds_run"):
        nonnegative(evidence.get(key), key)
    documents = nonnegative(manifest.get("document_count"), "document_count")
    batch = nonnegative(ready.get("changeset_size"), "changeset_size")
    if batch == 0:
        raise ValueError("changeset_size must be positive")
    if (evidence["documents_derived"] != documents or evidence["documents_upserted"] != documents
            or evidence["document_opens"] < documents or evidence["documents_deleted"]
            or evidence["tombstones_recorded"] or evidence["recoveries_run"] or evidence["rebuilds_run"]):
        raise ValueError("fresh startup did not complete ordinary document work")
    validate_work(evidence, "fresh", documents, batch)


def validate_receipts(mode, stages, ready, manifest, *, stored_links=None, construction=None):
    """Validate execution proof before queries; recheck against the later store audit."""
    keys = [(row.get("startup_stage"), row.get("probe")) for row in stages]
    expected = expected_checkpoints(mode)
    if len(keys) != len(expected) or set(keys) != expected:
        raise ValueError("checkpoint coverage differs from the requested loading mode")
    construction_group(mode, construction)
    if mode == "health-once-txn":
        validate_construction_receipts(stages, ready, construction)
        stages = stages[:2]
        keys = keys[:2]
    if mode not in ("health-once", "health-once-txn"):
        return
    if keys != [("health-once-facts", "boundary"), ("health-once-health", "boundary")]:
        raise ValueError("health-once boundaries are out of order")
    if ready.get("ready") is not True:
        raise ValueError("health-once did not complete Ready")
    startup = nonnegative(ready.get("startup_complete_ns"), "startup_complete_ns")
    for key in ("attach_ns", "observed_ready_ns", "host_setup_ns"):
        if key in ready and nonnegative(ready[key], key) > startup:
            raise ValueError(f"{key} exceeds startup_complete_ns")
    validate_fresh_work(ready, manifest)
    previous_clock = -1
    works = []
    for index, stage in enumerate(stages):
        exact = {"loading_mode": mode, "mode_executed": True,
                 "simulation": "private-initial-build", "global_ready": False,
                 "queries_executed": False, "health_complete": bool(index)}
        for key, value in exact.items():
            if type(stage.get(key)) is not type(value) or stage.get(key) != value:
                raise ValueError(f"invalid health-once {key} receipt")
        if nonnegative(stage.get("query_host_ns"), "query_host_ns") != 0 or any(
                key in stage for key in ("command", "answer", "query_started_ns", "query_finished_ns")):
            raise ValueError("health-once must not execute checkpoint queries")
        clock = nonnegative(stage.get("stage_boundary_ns"), "stage_boundary_ns")
        if not previous_clock < clock < startup:
            raise ValueError("health-once boundaries must increase within startup_complete_ns")
        previous_clock = clock
        work = stage.get("health_once_work")
        if not isinstance(work, dict) or not set(WRITER_WORK) <= work.keys():
            raise ValueError("health-once work receipt is incomplete")
        for key, value in work.items():
            nonnegative(value, key)
        works.append(work)
    facts, final = works
    if any(facts[key] for key in HEALTH_WORK):
        raise ValueError("ingestion performed link health decisions")
    for key in WRITER_WORK:
        if final[key] < facts[key] or (key in DOCUMENT_WORK and final[key] != facts[key]):
            raise ValueError("final health changed document work or lost accumulated work")
        if ready["job_evidence"][key] != final[key]:
            raise ValueError("final health work disagrees with the Ready account")
    if stored_links is not None and final["links_redecided"] != nonnegative(stored_links, "stored links"):
        raise ValueError("final health did not judge each stored link exactly once")


CONSTRUCTION_KEYS = ("group", "commit_attempts", "commits", "rollback_attempts", "rollbacks",
    "automatic_rollbacks", "document_changesets", "finding_units", "cleanup_units", "health_resets", "health_generation_groups",
    "drain_changesets", "drain_cleanup_units", "drain_finding_units", "commits_documents", "commits_findings", "commits_cleanup", "commits_health_reset", "commits_health_generation", "commits_drain")


def validate_construction_receipts(stages, ready, group):
    expected_order = ["health-once-facts", "health-once-health", "construction-complete"]
    if [s.get("startup_stage") for s in stages] != expected_order:
        raise ValueError("construction boundaries are out of order")
    expected_group = 0 if group == "unbounded" else int(group)
    previous = None
    for index, stage in enumerate(stages):
        work = stage.get("construction_work" if index == 2 else "health_once_work", {})
        if not isinstance(work, dict):
            raise ValueError("construction work receipt is missing")
        r = {k: nonnegative(work.get("construction_" + k), "construction_" + k) for k in CONSTRUCTION_KEYS}
        if r["group"] != expected_group or r["commit_attempts"] != r["commits"]:
            raise ValueError("construction group or successful commits disagree")
        if any(r[k] for k in ("rollback_attempts", "rollbacks", "automatic_rollbacks")):
            raise ValueError("completed construction contains a rollback")
        if r["commits"] != sum(r["commits_" + p] for p in ("documents", "findings", "cleanup", "health_reset", "health_generation", "drain")):
            raise ValueError("construction phase commits disagree")
        for phase, units in (("documents", "document_changesets"), ("findings", "finding_units"),
                ("cleanup", "cleanup_units"), ("health_reset", "health_resets"),
                ("health_generation", "health_generation_groups")):
            if r["commits_" + phase] > r[units] or (
                    expected_group == 1 and r["commits_" + phase] != r[units]):
                raise ValueError("construction phase commits disagree with completed units")
        if r["commits_drain"] != r["drain_changesets"] + r["drain_cleanup_units"] + r["drain_finding_units"]:
            raise ValueError("ordinary drain units must commit individually")
        if index < 2 and r["drain_changesets"] + r["drain_cleanup_units"] + r["drain_finding_units"] + r["commits_drain"]:
            raise ValueError("watcher drain preceded construction commit")
        if index == 0 and (r["health_resets"] or r["health_generation_groups"]):
            raise ValueError("final health preceded full facts")
        if r["document_changesets"] != stages[0]["health_once_work"]["changesets_applied"]:
            raise ValueError("construction changed logical document changesets")
        facts_units = r["document_changesets"] + r["finding_units"] + r["cleanup_units"]
        if index == 0:
            expected_commits = 0 if expected_group == 0 else facts_units // expected_group
            if r["commits"] != expected_commits:
                raise ValueError("facts commits disagree with completed full groups")
        if index >= 1:
            if r["health_resets"] != 1 or not 0 < r["health_generation_groups"] <= r["document_changesets"]:
                raise ValueError("final health generation account is missing")
            units = facts_units + r["health_resets"] + r["health_generation_groups"]
            # Sanity-check independently recorded counts; norn-db tests also
            # reconcile successful commits against actual SQLite execution.
            expected_commits = 1 if expected_group == 0 else (units + expected_group - 1) // expected_group
            if r["commits"] != expected_commits + r["commits_drain"]:
                raise ValueError("construction commits disagree with bounded units")
            # Facts precede the single reset, then generation units are contiguous.
            # Only their group endpoints (including the final partial) can commit.
            facts_commits = 0 if expected_group == 0 else facts_units // expected_group
            reset_commits = int(expected_group != 0 and (facts_units + 1) % expected_group == 0)
            if (r["commits_health_reset"] != reset_commits or
                    r["commits_health_generation"] != expected_commits - facts_commits - reset_commits):
                raise ValueError("final health commits disagree with reset and generation order")
        if previous is not None:
            for key in CONSTRUCTION_KEYS:
                if r[key] < previous[key]:
                    raise ValueError("construction receipt lost work")
                if index == 1 and key in ("document_changesets", "finding_units", "cleanup_units",
                        "commits_documents", "commits_findings", "commits_cleanup") and r[key] != previous[key]:
                    raise ValueError("final health changed completed facts units or phase commits")
                if index == 2 and key not in ("commit_attempts", "commits", "drain_changesets",
                        "drain_cleanup_units", "drain_finding_units", "commits_drain") and r[key] != previous[key]:
                    raise ValueError("completed health changed construction units or phase commits")
        previous = r
    final = stages[2]
    for key, value in {"loading_mode": "health-once-txn", "mode_executed": True,
            "queries_executed": False, "health_complete": True, "global_ready": False,
            "simulation": "private-initial-build"}.items():
        if type(final.get(key)) is not type(value) or final.get(key) != value:
            raise ValueError("invalid final construction boundary")
    if final.get("query_host_ns") != 0 or any(key in final for key in ("answer", "command", "query_started_ns", "query_finished_ns")):
        raise ValueError("construction boundary executed a query")
    if not stages[1]["stage_boundary_ns"] < nonnegative(final.get("stage_boundary_ns"), "final boundary") < ready["startup_complete_ns"]:
        raise ValueError("final construction boundary clock is invalid")
    if ready.get("changeset_size") != 128:
        raise ValueError("transaction experiment fixes document changesets at 128")


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


def cold_equivalence(result):
    """Compare logical rows, construction, whole Get and ordinary work, not page maps."""
    return {"semantic_sha256": result["store"]["semantic_sha256"],
            "document_construction_sha256": result["store"]["document_construction_sha256"],
            "whole_get": result["first_get"]["normalized_answer"],
            "ordinary_work": {key: result["ready"]["job_evidence"][key] for key in ORDINARY_WORK}}


def compare_cold_arms(reference, result):
    actual = cold_equivalence(result)
    for key, value in reference.items():
        if actual[key] != value:
            raise ValueError(f"cold arms differ in {key}")


def run_one(binary, root, manifest, mode, *, profiling=True, construction=None):
    """Keep query validation in readiness clocks and persisted-state audit outside them."""
    construction_group(mode, construction)
    began = time.perf_counter_ns()
    keys = []
    canonical = mode.startswith("canonical-")
    expected = expected_checkpoints(mode)
    root.mkdir(parents=True, exist_ok=True)

    def on_stage(stage):
        key = stage["startup_stage"], stage["probe"]
        if mode.endswith("-silent") or mode in ("health-once", "health-once-txn"):
            if key not in expected or stage.get("queries_executed") is not False:
                raise ValueError("a silent control emitted a query claim")
        else:
            validate_stage(stage, manifest)
        if key in keys:
            raise ValueError("duplicate checkpoint")
        keys.append(key)
        stage["observed_after_launch_ns"] = time.perf_counter_ns() - began

    worker = Current(binary, root, profiling=profiling,
                     loading=None if mode == "baseline" else mode, stage_handler=on_stage,
                     **({"construction_group": construction} if construction is not None else {}))
    try:
        ready_observed = time.perf_counter_ns() - began
        validate_receipts(mode, worker.stages, worker.ready, manifest, construction=construction)
        # The staged modes intentionally derive/replay extra documents; only
        # baseline and health-once are the ordinary-heal work comparison.
        if mode == "baseline":
            validate_fresh_work(worker.ready, manifest)
        with (root / "stages.jsonl").open("a") as log:
            for stage in worker.stages:
                stage["verified"] = True
                stage["validated_after_launch_ns"] = time.perf_counter_ns() - began
                append(log, stage)

        def sample():
            started = time.perf_counter_ns() - began
            reading = {"resources": resources(worker.process.pid), "load": os.getloadavg(),
                       "storage": storage_bytes(root)}
            finished = time.perf_counter_ns() - began
            return {"started_after_launch_ns": started, "finished_after_launch_ns": finished,
                    "sampling_ns": finished - started, **reading}

        before_first = sample()
        first_command = {"op": "get", "target": "small"}
        first, first_round_trip = worker.call(first_command)
        normalized_first = validate_first_get(first, manifest)
        first_validated = time.perf_counter_ns() - began
        after_first = sample()
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
        later_checks_validated = time.perf_counter_ns() - began
        after_checks = sample()
        milestones = {}
        for stage in worker.stages:
            name = stage["startup_stage"]
            milestones[name] = min(milestones.get(name, stage["stage_boundary_ns"]), stage["stage_boundary_ns"])
        result = {"mode": mode, "construction_group": construction, "ready_observed_ns": ready_observed, "ready": worker.ready,
                  "checkpoint_boundaries_ns": milestones,
                  "probe_query_ns": sum(stage["query_host_ns"] for stage in worker.stages),
                  "stages": worker.stages, "checks": checked,
                  "first_get": {"command": first_command, "response": first,
                                "normalized_answer": normalized_first, "round_trip_ns": first_round_trip},
                  "first_validated_get_ns": first_validated,
                  "later_checks_validated_ns": later_checks_validated,
                  "samples": {"before_first_request": before_first,
                              "after_first_validated_get": after_first, "after_later_checks": after_checks},
                  "resource_scope": "main-process Linux observations during the recorded sampling intervals; not exact kernel-boundary samples",
                  "endpoint_scope": "launch through answer validation; includes receipt validation and all pre-Get sampling overhead"}
    finally:
        closing = time.perf_counter_ns()
        worker.close()
        closed = time.perf_counter_ns()
    result["shutdown_started_ns"] = closing - began
    result["closed_after_launch_ns"] = closed - began
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
    validate_receipts(mode, worker.stages, worker.ready, manifest,
                      stored_links=result["store"]["counts"]["links"], construction=construction)
    if mode not in ("baseline", "health-once", "health-once-txn") and not mode.endswith("-silent"):
        summary = next(s for s in result["stages"] if s["startup_stage"] == "health")
        total = sum(row["count"] for row in summary["answer"]["report"]["by_kind"])
        if total != result["store"]["counts"]["findings"]:
            raise ValueError("the health checkpoint preceded its complete findings")
    for stream in ("stdout", "stderr"):
        shutil.copy2(root / f"worker.{stream}.log", root / f"fresh.{stream}.log")
    # Health-once deliberately refuses all existing stores. Warm verification
    # uses ordinary loading; no experimental observer can touch that history.
    restart_loading = None if mode in ("baseline", "health-once", "health-once-txn") else mode
    restart = Current(binary, root, profiling=profiling, loading=restart_loading)
    try:
        if restart.stages:
            raise ValueError("unchanged restart entered the initial loading experiment")
        validate_work(restart.ready["job_evidence"], "restart", manifest["document_count"], 128)
        for _, command in READS:
            answer, _ = restart.call(command)
            validate_read("current", command, answer, manifest)
        result["restart"] = {"ready": restart.ready, "resources": resources(restart.process.pid),
                             "loading": restart_loading or "ordinary"}
    finally:
        restart.close()
    restarted = audit_store(binary, root, manifest)
    for key in ("store_epoch", "write_generation", "semantic_sha256", "document_construction_sha256"):
        if restarted[key] != result["store"][key]:
            raise ValueError("unchanged restart rebuilt or changed the final index")
    result["persisted_checks_completed_ns"] = time.perf_counter_ns() - began
    result["restart"]["verified"] = True
    result["verified"] = True
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sizes", nargs="+", type=int, default=[300, 2000, 10000])
    parser.add_argument("--modes", nargs="+", choices=["baseline", "health-once", "health-once-txn", "layered", "layered-silent", "facts-once", "facts-once-silent", "canonical-demand", "canonical-demand-silent", "canonical-intake", "canonical-intake-silent"], default=["baseline", "layered", "layered-silent"])
    parser.add_argument("--construction-groups", nargs="+", choices=["1", "16", "unbounded"])
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--no-profile", action="store_true", help="disable optional profiling spans in measured workers")
    args = parser.parse_args()
    if ("baseline" not in args.modes and "health-once-txn" not in args.modes) or len(set(args.modes)) != len(args.modes) or len(set(args.sizes)) != len(args.sizes):
        parser.error("include one baseline and use unique modes and sizes")
    if ("health-once-txn" in args.modes) != bool(args.construction_groups):
        parser.error("health-once-txn requires explicit --construction-groups (and only that mode uses them)")
    if args.construction_groups and len(set(args.construction_groups)) != len(args.construction_groups):
        parser.error("use unique construction groups")
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
                "profiling": not args.no_profile, "construction_groups": args.construction_groups,
                "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "load_start": os.getloadavg()}
    write_json(args.output / "run.json", metadata)
    references = {}
    construction_references = {}
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
                arms = [(mode, group) for mode in args.modes
                        for group in (args.construction_groups if mode == "health-once-txn" else [None])]
                modes = arms if repetition % 2 == 0 else list(reversed(arms))
                sizes = args.sizes[repetition % len(args.sizes):] + args.sizes[:repetition % len(args.sizes)]
                for size in sizes:
                    frozen, manifest = fixtures[size]
                    for mode, group in modes:
                        label = mode if group is None else f"{mode}-g{group}"
                        root = (args.output / "work" / f"r{repetition}-n{size}-{label}").resolve()
                        root.mkdir(parents=True)
                        shutil.copytree(frozen / "vault", root / "vault")
                        if tree_digest(root / "vault") != manifest["tree_sha256"]:
                            raise ValueError("copied fixture changed")
                        result = run_one(binary, root, manifest, mode, profiling=not args.no_profile, construction=group)
                        digest = result["store"]["semantic_sha256"]
                        if references.setdefault(size, digest) != digest:
                            raise ValueError("staging changed the final index")
                        if mode in ("baseline", "health-once", "health-once-txn"):
                            reference = construction_references.setdefault(size, cold_equivalence(result))
                            compare_cold_arms(reference, result)
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
