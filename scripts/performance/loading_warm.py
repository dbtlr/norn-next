#!/usr/bin/env python3
"""Compare verified warm workloads after ordinary and staged initial loading."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sys
import time
from types import SimpleNamespace


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("candidate-source", "candidate-build", "baseline-source", "baseline-build", "audit-build", "fixtures", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--sizes", nargs="+", type=int, default=[300, 2000, 10000])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=5)
    parser.add_argument("--samples", type=int, default=25)
    parser.add_argument("--modes", nargs="+", choices=["layered-silent", "facts-once-silent"], default=["layered-silent", "facts-once-silent"])
    args = parser.parse_args()
    source = args.candidate_source.resolve()
    baseline_source = args.baseline_source.resolve()
    output = args.output.resolve()
    if output.exists() or any(output.is_relative_to(root) for root in (source, baseline_source)):
        parser.error("use a new output directory outside both checkouts")
    if args.repetitions < 1 or args.samples < 1 or args.warmups < 0 or len(args.sizes) != len(set(args.sizes)):
        parser.error("positive repetitions/samples, nonnegative warmups and unique sizes required")
    sys.path.insert(0, str(source / "scripts/performance"))
    from baseline import prepare, run_worker, write_json
    from provenance import sha256, source_tree_digest, verify_build
    from startup_audit import audit_store
    from fixture import tree_digest

    receipts = {"candidate": verify_build(args.candidate_build, source),
                "baseline": verify_build(args.baseline_build, baseline_source)}
    if receipts["baseline"]["features"] or "induced-failure" not in receipts["candidate"]["features"]:
        raise ValueError("use a standard baseline and explicitly instrumented candidate")
    audit = verify_build(args.audit_build, source)
    if "induced-failure" not in audit["features"]:
        raise ValueError("audit worker requires its profiling feature")
    output.mkdir(parents=True)
    runner = output / "runner"
    runner.mkdir()
    for path in (source / "scripts/performance").glob("*.py"):
        shutil.copy2(path, runner / path.name)
    shutil.copy2(__file__, runner / "loading_warm.py")
    binaries = {}
    for revision, built in receipts.items():
        binaries[revision] = output / (revision + "-worker")
        shutil.copy2(built["executable"], binaries[revision])
        if sha256(binaries[revision]) != built["sha256"]:
            raise ValueError("frozen binary hash mismatch")
    audit_binary = output / "audit-worker"
    shutil.copy2(audit["executable"], audit_binary)
    if sha256(audit_binary) != audit["sha256"]:
        raise ValueError("frozen audit binary hash mismatch")
    metadata = {"completed": False, "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "workload_version": "runtime-baseline-v1", "builds": receipts, "audit_build": audit,
                "runner_source_sha256": source_tree_digest(source), "wrapper_sha256": sha256(Path(__file__)),
                "sizes": args.sizes, "modes": args.modes, "repetitions": args.repetitions, "warmups": args.warmups,
                "samples": args.samples, "fixture_manifests": {}, "load_start": os.getloadavg()}
    write_json(output / "run.json", metadata)
    try:
        fixtures = {}
        for size in args.sizes:
            fixtures[size] = prepare(SimpleNamespace(output=output, fixtures=args.fixtures), size)
            metadata["fixture_manifests"][str(size)] = fixtures[size][1]
        final_hashes = {}
        final_semantics = {}
        for repetition in range(args.repetitions):
            sizes = args.sizes[repetition % len(args.sizes):] + args.sizes[:repetition % len(args.sizes)]
            revisions = ["baseline", "candidate", *args.modes]
            if repetition % 2:
                revisions.reverse()
            for size in sizes:
                fixture, manifest = fixtures[size]
                for revision in revisions:
                    folder = output / revision
                    folder.mkdir(exist_ok=True)
                    with (folder / "samples.jsonl").open("a", encoding="utf-8") as samples:
                        run_worker(SimpleNamespace(output=folder, current=binaries["candidate" if revision in args.modes else revision],
                                                   loading=revision if revision in args.modes else None,
                                                   samples=args.samples, warmups=args.warmups), "current", size, repetition,
                                   fixture, manifest, samples)
                    # Compare complete final source trees before deleting only measured cell state.
                    records = [json.loads(line) for line in (folder / "samples.jsonl").read_text().splitlines()]
                    end = records[-1]
                    if end["kind"] != "end" or final_hashes.setdefault(size, end["final_tree_sha256"]) != end["final_tree_sha256"]:
                        raise ValueError("final source trees differ across revisions or repetitions")
                    root = folder / "work" / f"{size}-{repetition}-c"
                    vault = root / "vault"
                    paths = sorted(path.relative_to(vault).as_posix() for path in vault.rglob("*.md"))
                    final_manifest = {"paths": paths, "document_count": len(paths),
                                      "markdown_bytes": sum((vault / path).stat().st_size for path in paths),
                                      "tree_sha256": tree_digest(vault)}
                    final_store = audit_store(audit_binary, root, final_manifest)
                    if final_semantics.setdefault(size, final_store["semantic_sha256"]) != final_store["semantic_sha256"]:
                        raise ValueError("final derived stores differ after the warm workload")
                    write_json(root / "final-audit.json", final_store)
                    write_json(root / "final-source.json", final_manifest)
                    for name in ("vault", "data", "cache", "state", "config"):
                        path = root / name
                        if path.exists():
                            shutil.rmtree(path)
        for revision, root in (("candidate", source), ("baseline", baseline_source)):
            if source_tree_digest(root) != receipts[revision]["source_tree_sha256"]:
                raise ValueError("source changed during warm measurement")
        metadata["completed"] = True
    except BaseException as problem:
        metadata["failure"] = repr(problem)
        raise
    finally:
        metadata["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        metadata["load_end"] = os.getloadavg()
        write_json(output / "run.json", metadata)


if __name__ == "__main__":
    main()
