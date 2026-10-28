#!/usr/bin/env python3
"""Measure initial indexing and existing-index restart on frozen baseline fixtures."""

import argparse
import json
import os
import platform
import shutil
import sys
import time
from pathlib import Path

from baseline import READS, append, write_json
from fixture import COMMON, tree_digest
from provenance import sha256, source_tree_digest, verify_build
from results import validate_read
from transport import Current, resources


def baseline_build(receipt: Path, source: Path) -> dict:
    """Verify the reference revision independently of the candidate checkout."""
    built = verify_build(receipt, source.resolve())
    if built["features"]:
        raise ValueError("the revision baseline must be a no-feature build")
    return built


def storage_bytes(root: Path) -> dict:
    """Read file lengths while the worker still holds its database and WAL open."""
    stores = list((root / "data").rglob("store.sqlite3"))
    if len(stores) != 1:
        raise ValueError("storage readings require exactly one store.sqlite3")
    database = stores[0]
    paths = {"database": database, "wal": database.with_name(database.name + "-wal"),
             "shared_memory": database.with_name(database.name + "-shm")}
    return {name: path.stat().st_size if path.exists() else 0 for name, path in paths.items()}


def partition(ready: dict) -> dict | None:
    """Partition inclusive spans once; retain nested detail separately in raw evidence."""
    timings = ready["timings"]
    if timings is None:
        return None
    if timings["unknown_labels"] or timings["unfinished_spans"]:
        raise ValueError("startup timing is incomplete")
    values = {entry["label"]: entry["elapsed_ns"] for entry in timings["entries"]}
    if len(values) != len(timings["entries"]) or any(value < 0 for value in values.values()):
        raise ValueError("duplicate or negative phase readings")
    plan, increment = values["host.document_plan"], values["store.increment"]
    rows, health, commit = (values[name] for name in
                            ("store.upsert", "store.link_health", "store.commit"))
    once = values["store.link_health_once"]
    full_text = values.get("store.step.full_text", 0) + values.get("store.step.finish", 0)
    content = values.get("host.step.content", 0) + values.get("store.step.content", 0)
    structure = values.get("host.step.structure", 0)
    result = {"content": content, "structure": structure, "full_text": full_text, "link_health_once": once, "document_plan": plan, "row_and_index_writes": rows,
              "link_health": health, "commit": commit,
              "other_increment": increment - rows - health - commit,
              "other_startup": ready["startup_complete_ns"] - plan - increment - once - full_text - content - structure}
    if any(value < 0 for value in result.values()):
        raise ValueError("timing scopes overlap or exceed the startup clock")
    return result


def validate_work(evidence, stage, documents, batch, store):
    """Reject timings made cheaper by skipped import work or a changed restart."""
    if evidence is None:
        return
    if stage == "fresh":
        for key in ("documents_named", "content_rows_written", "structure_rows_written", "documents_read_by_build"):
            if evidence[key] != documents:
                raise ValueError(f"fresh startup {key} does not equal its document population")
        if evidence["fact_rows_discarded"] or evidence["links_redecided"]:
            raise ValueError("fresh startup discarded facts or re-decided links")
    if stage == "fresh" and evidence["links_judged"] != store["tables"]["links"]["rows"]:
        raise ValueError("fresh startup did not judge exactly every stored link")
    if stage == "restart" and any(evidence[key] for key in
                                  ("recoveries_run", "rebuilds_run", "documents_derived", "documents_upserted")):
        raise ValueError("unchanged restart unexpectedly recovered or derived documents")


def run_one(binary: Path, audit_binary: Path, root: Path, manifest: dict, batch: int,
            build_kind: str, profiling: bool, stage: str) -> dict:
    """Time the validated worker lifecycle, keeping the persisted-state audit outside it."""
    from startup_audit import audit_store

    load = os.getloadavg()
    began = time.perf_counter_ns()
    worker = Current(binary, root, changeset_size=batch, profiling=profiling)
    try:
        first, query_ns = worker.call({"op": "search", "query": COMMON})
        validate_read("current", {"op": "search", "query": COMMON}, first, manifest)
        elapsed = time.perf_counter_ns() - began
        at_first_query = resources(worker.process.pid)
        disk_at_first_query = storage_bytes(root)
        ready = worker.ready
        if ready["changeset_size"] != batch or ready["store_page_size"] != 128:
            raise ValueError("worker did not use the requested policy")
        if bool(ready["timings"]) != profiling or (ready["job_evidence"] is not None) != (build_kind == "profile"):
            raise ValueError("worker does not expose the requested profiling controls")
        phases = partition(ready)
        verified = []
        for name, command in READS:
            answer, _ = worker.call(command)
            validate_read("current", command, answer, manifest)
            verified.append(name)
        result = {"stage": stage, "first_validated_query_ns": elapsed,
                  "first_query_round_trip_ns": query_ns, "first_query_host_ns": first["host_ns"],
                  "first_query_counters": first["counters"], "ready": ready, "partition_ns": phases,
                  "resources_at_first_query": at_first_query, "resources_after_checks": resources(worker.process.pid),
                  "storage_bytes_at_first_query": disk_at_first_query,
                  "verified_reads": verified, "load_start": load, "load_end": os.getloadavg()}
    finally:
        closing = time.perf_counter_ns()
        worker.close()
        closed = time.perf_counter_ns()
    result["shutdown_ns"] = closed - closing
    result["validated_lifecycle_ns"] = closed - began
    result["storage_bytes_after_close"] = storage_bytes(root)
    for name in ("worker.stdout.log", "worker.stderr.log"):
        shutil.copyfile(root / name, root / (stage + "." + name))
    result["store"] = audit_store(audit_binary, root, manifest)
    validate_work(ready["job_evidence"], stage, manifest["document_count"], batch, result["store"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current-build", type=Path, required=True)
    parser.add_argument("--control-build", type=Path,
                        help="interleave a standard no-feature build from the same source")
    parser.add_argument("--baseline-build", type=Path,
                        help="interleave a no-feature build from an earlier revision")
    parser.add_argument("--baseline-source", type=Path,
                        help="checkout matching the baseline build receipt")
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sizes", type=int, nargs="+", default=[300, 2000, 10000])
    parser.add_argument("--batches", type=int, nargs="+", default=[128, 512, 1024])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--timings", choices=["on", "off", "both"], default="on")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    args.output = args.output.resolve()
    if sys.platform != "linux":
        parser.error("startup resource readings require Linux")
    if args.output.exists() or args.output.is_relative_to(repo):
        parser.error("use a new output directory outside the repository")
    if bool(args.baseline_build) != bool(args.baseline_source):
        parser.error("--baseline-build and --baseline-source are required together")
    if args.baseline_build and not args.control_build:
        parser.error("a revision comparison also requires --control-build")
    if args.baseline_source and args.output.is_relative_to(args.baseline_source.resolve()):
        parser.error("the output directory must also be outside the baseline checkout")
    if args.repetitions < 1 or any(not 1 <= batch <= 1024 for batch in args.batches):
        parser.error("positive repetitions and changeset sizes between 1 and 1024 are required")
    if len(set(args.sizes)) != len(args.sizes) or len(set(args.batches)) != len(args.batches):
        parser.error("sizes and batches must be unique")
    receipt = verify_build(args.current_build, repo)
    if "induced-failure" not in receipt["features"]:
        parser.error("build with provenance.py --startup-profile first")
    args.output.mkdir(parents=True)
    runner = args.output / "runner"
    runner.mkdir()
    for path in Path(__file__).parent.glob("*.py"):
        shutil.copy2(path, runner / path.name)
    receipts = {"profile": receipt}
    if args.control_build is not None:
        control = verify_build(args.control_build, repo)
        if control["features"] or control["source_tree_sha256"] != receipt["source_tree_sha256"]:
            raise ValueError("control must be a no-feature build from the same source tree")
        receipts["standard"] = control
    if args.baseline_build is not None:
        receipts["baseline"] = baseline_build(args.baseline_build, args.baseline_source)
    binaries = {}
    for kind, built in receipts.items():
        binaries[kind] = args.output / (kind + "-worker")
        shutil.copy2(built["executable"], binaries[kind])
        if sha256(binaries[kind]) != built["sha256"]:
            raise ValueError("frozen executable differs from build receipt")
    modes = [True, False] if args.timings == "both" else [args.timings == "on"]
    variants = [{"build_kind": "profile", "profiling": mode} for mode in modes]
    if "standard" in binaries:
        variants.append({"build_kind": "standard", "profiling": False})
    if "baseline" in binaries:
        variants.append({"build_kind": "baseline", "profiling": False})
    metadata = {"workload_version": "startup-attribution-v2", "completed": False,
                "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "current_build": receipt, "builds": receipts, "runner_source_sha256": source_tree_digest(repo),
                "sizes": args.sizes, "batches": args.batches, "repetitions": args.repetitions,
                "variants": variants, "store_page_size": 128,
                "platform": platform.platform(), "cpu_count": os.cpu_count(),
                "load_start": os.getloadavg(), "fixture_manifests": {}}
    write_json(args.output / "run.json", metadata)
    try:
        fixtures = {}
        for size in args.sizes:
            source = args.fixtures / str(size)
            manifest = json.loads((source / "manifest.json").read_text())
            if manifest["background_count"] != size or manifest["workload_version"] != "runtime-baseline-v1":
                raise ValueError("fixture identity differs from the baseline workload")
            if tree_digest(source / "vault") != manifest["tree_sha256"]:
                raise ValueError("fixture bytes differ from their manifest")
            frozen = args.output / "fixtures" / str(size)
            frozen.mkdir(parents=True)
            shutil.copytree(source / "vault", frozen / "vault")
            write_json(frozen / "manifest.json", manifest)
            fixtures[size] = (frozen, manifest)
            metadata["fixture_manifests"][str(size)] = manifest
        references = {}
        with (args.output / "samples.jsonl").open("w") as samples:
            for repetition in range(args.repetitions):
                sizes = args.sizes[repetition % len(args.sizes):] + args.sizes[:repetition % len(args.sizes)]
                for size in sizes:
                    source, manifest = fixtures[size]
                    batches = args.batches[repetition % len(args.batches):] + args.batches[:repetition % len(args.batches)]
                    for batch in batches:
                        for variant in (variants if repetition % 2 == 0 else list(reversed(variants))):
                            profiling, build_kind = variant["profiling"], variant["build_kind"]
                            root = args.output / "work" / f"r{repetition}-n{size}-b{batch}-{build_kind}-p{int(profiling)}"
                            root.mkdir(parents=True)
                            shutil.copytree(source / "vault", root / "vault")
                            if tree_digest(root / "vault") != manifest["tree_sha256"]:
                                raise ValueError("copied fixture differs from frozen source")
                            context = {"repetition": repetition, "background_documents": size,
                                       "document_count": manifest["document_count"], "changeset_size": batch,
                                       "build_kind": build_kind, "profiling": profiling, "fixture_sha256": manifest["tree_sha256"]}
                            previous = None
                            for stage in ("fresh", "restart"):
                                result = run_one(binaries[build_kind], binaries["profile"], root, manifest,
                                                 batch, build_kind, profiling, stage)
                                semantic = result["store"]["semantic_sha256"]
                                if references.setdefault(size, semantic) != semantic:
                                    raise ValueError("different configurations produced different semantic stores")
                                if previous is not None:
                                    for key in ("store_epoch", "write_generation"):
                                        if result["store"][key] != previous[key]:
                                            raise ValueError("unchanged restart rebuilt or wrote derived state")
                                previous = result["store"]
                                append(samples, {**context, **result, "verified": True})
                                print(f"{manifest['document_count']} documents, batch {batch}, {build_kind}, timing {profiling}, {stage}: {result['first_validated_query_ns'] / 1e9:.3f}s verified", flush=True)
                            # Retain logs and audits; one frozen corpus can recreate every cell.
                            for name in ("vault", "data", "cache", "state", "config"):
                                path = root / name
                                if path.exists():
                                    shutil.rmtree(path)
        if source_tree_digest(repo) != metadata["runner_source_sha256"]:
            raise ValueError("source changed during measurement; preserve this run as incomplete")
        if args.baseline_source and source_tree_digest(args.baseline_source) != receipts["baseline"]["source_tree_sha256"]:
            raise ValueError("baseline source changed during measurement")
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
