#!/usr/bin/env python3
"""Paused observer residency attribution only. Clocks are not performance acceptance."""
import json
import time
from pathlib import Path

from transport import Current, resources

CHECKPOINTS = ("before-early-get", "after-early-get", "before-metadata", "after-metadata",
               "intake-retired-before-final", "after-final", "at-ready-before-reads",
               "after-common-reads")
METADATA = [("active_page", {"op": "find"}), ("document_count", {"op": "count"}),
            ("active_count", {"op": "count", "active": True}),
            ("sparse_page", {"op": "find", "value": "probe"}),
            ("zero_page", {"op": "find", "value": "profilenonexistentvalue"}),
            ("unknown_field", {"op": "find", "key": "profilenonexistentkey"}),
            ("field_page", {"op": "find", "value": "probe", "field_sort": True})]


def expected_probes(cell):
    if cell not in "NGEQM" or len(cell) != 1:
        raise ValueError("unknown residency history")
    probes = [("catalogue", "names", None)]
    if cell != "N":
        probes.append(("canonical-demand", "whole_slug", {"op": "get", "target": "small", "whole": True}))
    if cell in "EQM":
        probes += [("canonical-intake", name, command) for name, command in METADATA]
    if cell == "Q":
        probes += [("canonical-health", "whole_slug", {"op": "get", "target": "small", "whole": True})]
        probes += [("canonical-final-intake", name, command) for name, command in METADATA]
        probes += [("frontmatter", name, command) for name, command in METADATA[:3]]
        probes += [("lexical", name, command) for name, command in [
            ("common", {"op": "search", "query": "profilecommonword"}),
            ("rare", {"op": "search", "query": "profileraresignal"}),
            ("missing", {"op": "search", "query": "profilenonexistenttoken"}),
            ("filtered", {"op": "search", "query": "profilecommonword", "path": "bench/**"}),
            ("coverage", {"op": "count", "matches": "profilecommonword"})]]
        probes += [("rich", "section", {"op": "get", "target": "bench/large.md#Details"}),
                   ("health", "summary", {"op": "validate"})]
    return probes


def validate_ack(ack, sequence, checkpoint):
    if (not isinstance(ack, dict) or type(ack.get("sequence")) is not int or ack["sequence"] != sequence
            or ack.get("checkpoint") != checkpoint):
        raise ValueError("invalid residency checkpoint acknowledgement")


def validate_history(cell, events):
    """Validate protocol before inspecting answers; duplicate probes never collapse into a set."""
    checkpoints = [e for e in events if "residency_checkpoint" in e]
    if [e["residency_checkpoint"] for e in checkpoints] != list(CHECKPOINTS):
        raise ValueError("missing, duplicate or unexpected residency checkpoint")
    worker, attachment = checkpoints[0]["worker_tid"], checkpoints[0]["attachment_tid"]
    if type(worker) is not int or type(attachment) is not int or worker <= 0 or attachment <= 0 or worker == attachment:
        raise ValueError("worker and attachment identities must differ")
    previous = -1
    for sequence, event in enumerate(checkpoints, 1):
        if (type(event["sequence"]) is not int or event["sequence"] != sequence
                or event.get("construction_eligibility_gets") != 1 or event["worker_tid"] != worker
                or event["attachment_tid"] != attachment or event["thread_tid"] != worker
                or not previous < event["paused_ns"] <= event["acknowledged_ns"]
                or not event["parent_received_ns"] <= event["parent_sample_started_ns"]
                <= event["parent_sample_finished_ns"] <= event["parent_ack_published_ns"] <= event["parent_resumed_ns"]):
            raise ValueError("invalid checkpoint identity, sequence or sampling/ack order")
        previous = event["acknowledged_ns"]
    queries = [e for e in events if "startup_stage" in e]
    actual = [(e["startup_stage"], e["probe"], e.get("command")) for e in queries]
    if actual != expected_probes(cell):
        raise ValueError("missing, duplicate, unexpected or changed observer probes")
    previous_query = -1
    for event in queries:
        stage = event["startup_stage"]
        selected = worker if cell == "M" and stage != "catalogue" else attachment
        low, high = ((-1, checkpoints[0]["paused_ns"]) if stage == "catalogue" else
                     (checkpoints[0]["acknowledged_ns"], checkpoints[1]["paused_ns"])
                     if stage == "canonical-demand" else
                     (checkpoints[2]["acknowledged_ns"], checkpoints[3]["paused_ns"])
                     if stage == "canonical-intake" else
                     (checkpoints[4]["acknowledged_ns"], checkpoints[5]["paused_ns"]))
        if (event["thread_tid"] != selected or not
                low <= event["query_started_ns"] <= event["query_finished_ns"] <= high
                or event["query_started_ns"] < previous_query):
            raise ValueError("query ran on the wrong thread or outside acknowledged boundaries")
        previous_query = event["query_finished_ns"]
    reads = [e for e in events if "diagnostic_common_index" in e]
    from baseline import READS
    if [(e["diagnostic_common_index"], e["command"]) for e in reads] != list(enumerate(command for _, command in READS)):
        raise ValueError("missing, duplicate, unexpected or changed common reads")
    previous_read = checkpoints[6]["acknowledged_ns"]
    for read in reads:
        if (read["thread_tid"] != worker or not previous_read <= read["query_started_ns"]
                <= read["query_finished_ns"] <= checkpoints[7]["paused_ns"]):
            raise ValueError("ordinary read outside acknowledged Ready and final residency endpoints")
        previous_read = read["query_finished_ns"]


def validate_answers(cell, events, manifest, reference):
    """Run only after the last residency endpoint; preserve full normalized wire results."""
    from loading import validate_stage, whole_get_answer, metadata_answer
    validate_history(cell, events)
    for event in events:
        if "startup_stage" not in event:
            continue
        validate_stage(event, manifest)
        key = event["startup_stage"], event["probe"]
        if key[0] != "catalogue" and (not isinstance(event.get("projection_epoch"), str)
                or not event["projection_epoch"] or type(event.get("projection_generation")) is not int
                or event["projection_generation"] < 0):
            raise ValueError("missing or invalid query projection provenance")
        if key[0] == "canonical-demand":
            if whole_get_answer(event) != reference["get"]:
                raise ValueError("complete early Get differs from matched Q final answer")
        elif key[0] == "canonical-intake":
            if metadata_answer(event) != reference["metadata"][key[1]]:
                raise ValueError("complete early metadata differs from matched Q final answer")


def linux_residency(pid, proc_root=Path("/proc")):
    """Process mappings describe residency, never allocator ownership."""
    proc = proc_root / str(pid)
    result = resources(pid) if proc_root == Path("/proc") else {}
    totals = dict(mapping_rss_bytes=0, anonymous_pages_bytes=0, file_mapping_rss_bytes=0,
                  anonymous_mapping_rss_bytes=0, other_mapping_rss_bytes=0)
    category = "other_mapping_rss_bytes"
    for line in (proc / "smaps").read_text().splitlines():
        fields = line.split()
        if fields and "-" in fields[0] and len(fields) >= 5:
            name = fields[5] if len(fields) > 5 else ""
            category = ("file_mapping_rss_bytes" if name.startswith("/") else
                        "anonymous_mapping_rss_bytes" if not name or name.startswith(("[heap]", "[stack", "[anon:"))
                        else "other_mapping_rss_bytes")
        elif fields and fields[0] == "Rss:":
            value = int(fields[1]) * 1024
            totals["mapping_rss_bytes"] += value
            totals[category] += value
        elif fields and fields[0] == "Anonymous:":
            totals["anonymous_pages_bytes"] += int(fields[1]) * 1024
    for line in (proc / "status").read_text().splitlines():
        key, _, value = line.partition(":")
        if key in ("RssAnon", "RssFile", "RssShmem"):
            totals[key + "_bytes"] = int(value.split()[0]) * 1024
    tids = sorted(int(p.name) for p in (proc / "task").iterdir())
    return result | totals | {"thread_ids": tids, "interpretation": "residency only; anonymous maps do not identify an allocator"}


class PausedCurrent(Current):
    """Reuse the existing process lifecycle, adding only explicit pause receipts."""
    def __init__(self, binary, root, cell):
        self.events = []
        self.canonical_work = None
        self.run_root = root
        (root / "residency-acks").mkdir()
        super().__init__(binary, root, profiling=False, residency=cell)

    def receive(self):
        while True:
            event = super().receive()
            if "residency_canonical_work" in event:
                if self.canonical_work is not None:
                    raise ValueError("duplicate ordinary writer receipt")
                self.canonical_work = event["residency_canonical_work"]
                continue
            if "residency_checkpoint" in event:
                event["parent_received_ns"] = time.perf_counter_ns()
                event["parent_sample_started_ns"] = time.perf_counter_ns()
                event["resources"] = linux_residency(self.process.pid)
                event["parent_sample_finished_ns"] = time.perf_counter_ns()
                if (event["sequence"] != 1 + sum("residency_checkpoint" in e for e in self.events)
                        or event["residency_checkpoint"] not in CHECKPOINTS):
                    raise ValueError("unexpected pause sequence")
                if event["worker_tid"] != self.process.pid or event["attachment_tid"] not in event["resources"]["thread_ids"]:
                    raise ValueError("checkpoint thread identity is absent from the process")
                ack = dict(sequence=event["sequence"], checkpoint=event["residency_checkpoint"])
                validate_ack(ack, event["sequence"], event["residency_checkpoint"])
                destination = self.run_root / "residency-acks" / f'{event["sequence"]}.json'
                pending = destination.with_suffix(".pending")
                pending.write_text(json.dumps(ack) + "\n")
                pending.replace(destination)
                event["parent_ack_published_ns"] = time.perf_counter_ns()
                resumed = super().receive()
                if resumed.get("residency_acknowledged") != event["sequence"]:
                    raise ValueError("continuation or query preceded checkpoint acknowledgement")
                event["parent_resumed_ns"] = time.perf_counter_ns()
                event["acknowledged_ns"] = resumed["acknowledged_ns"]
                self.events.append(event)
                continue
            if "startup_stage" in event:
                self.events.append(event)
            return event


def run_cell(binary, root, manifest, cell):
    """No semantic validation query can precede the last residency endpoint."""
    from baseline import READS
    from results import validate_read
    from startup_audit import audit_store
    from transport import PREFIX, environment, stop_process
    import subprocess
    reads = []
    worker = PausedCurrent(binary, root, cell)
    try:
        for name, command in READS:
            answer, _ = worker.call(command)
            reads.append(dict(name=name, command=command, response=answer))
            worker.events.append({key: answer[key] for key in
                ("diagnostic_common_index", "thread_tid", "query_started_ns", "query_finished_ns")} | {"command": command})
        endpoint, _ = worker.call({"op": "residency-endpoint"})
        if endpoint["answer"] != {"residency_endpoint": True}:
            raise ValueError("worker omitted its final residency endpoint")
        validate_history(cell, worker.events)
        # The complete answers are retained only by this outside-process controller.
        for read in reads:
            if read["response"]["thread_tid"] != worker.process.pid:
                raise ValueError("ordinary reads left the worker main thread")
            validate_read("current", read["command"], read["response"], manifest)
        result = dict(cell=cell, events=worker.events, common_reads=reads,
                      canonical_work=worker.canonical_work, ready=worker.ready,
                      purpose="residency attribution only; clocks are not performance acceptance")
    finally:
        worker.close()
    # Integrity and persisted source/identity/layout audits run in separate processes
    # after the final endpoint and after all subject threads have terminated.
    with (root / "integrity.stdout.log").open("w") as stdout, (root / "integrity.stderr.log").open("w") as stderr:
        process = subprocess.Popen([str(binary), "verify"], cwd=root,
            env=environment(root, (root / "performance-token").read_text()),
            stdout=stdout, stderr=stderr, start_new_session=True)
        try:
            if process.wait(timeout=300) != 0:
                raise RuntimeError("post-endpoint integrity verification failed")
        finally:
            stop_process(process)
    if (root / "integrity.stdout.log").read_text().strip() != PREFIX + '{"integrity_verified":true}':
        raise ValueError("integrity receipt missing")
    result["store"] = audit_store(binary, root, manifest)
    return result


def execution_plan(repetitions=2):
    from baseline import READS
    if repetitions < 2:
        raise ValueError("residency attribution requires reversed-order repetitions")
    return dict(schema_version=1, purpose="residency attribution only; no cold or warm performance acceptance",
                base_commit="23542d216becc2fc99041a7d8ec867fe7d06fec7", serial=True,
                same_binary=True, frozen_fixture_document_count=2014,
                allocator="default; no tuning", profiling=False,
                invocation=["python3", "scripts/performance/residency.py", "--build", "{build_receipt}",
                            "--fixture", "{frozen_fixture}", "--output", "{new_run_directory}"],
                observer_probes={cell: expected_probes(cell) for cell in "NGEQM"},
                catalogue="identical three candidate probes on attachment thread in every cell",
                construction_eligibility_gets_per_cell=1,
                checkpoints=list(CHECKPOINTS),
                common_reads=READS,
                limitations=["paused frozen copy; no concurrent heal or live source-movement guarantee",
                             "anonymous residency does not identify allocator ownership",
                             "budget census unavailable at observer seam; no production API added"],
                runs=[dict(repetition=r, cell=cell) for r in range(repetitions)
                      for cell in ("NGEQM" if r % 2 == 0 else "MQEGN")])


def main():
    import argparse
    import shutil
    import sys
    from baseline import write_json
    from fixture import tree_digest
    from provenance import sha256, source_tree_digest, verify_build
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-plan", type=Path)
    parser.add_argument("--build", type=Path)
    parser.add_argument("--fixture", type=Path, help="frozen fixture directory containing vault and manifest.json")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repetitions", type=int, default=2)
    args = parser.parse_args()
    plan = execution_plan(args.repetitions)
    if args.write_plan:
        args.write_plan.parent.mkdir(parents=True, exist_ok=True)
        with args.write_plan.open("x") as file:
            file.write(json.dumps(plan, indent=2) + "\n")
        return
    if not all((args.build, args.fixture, args.output)):
        parser.error("provide --write-plan, or --build --fixture --output")
    repo = Path(__file__).resolve().parents[2]
    receipt = verify_build(args.build, repo)
    if "induced-failure" not in receipt["features"]:
        parser.error("residency attribution requires an induced-failure worker")
    if args.output.exists() or args.output.resolve().is_relative_to(repo):
        parser.error("choose a new output directory outside this checkout")
    # No ambient allocator experiment may silently change the comparison.
    import os
    if any(key.startswith(("MALLOC_", "JEMALLOC_", "MIMALLOC_", "TCMALLOC_")) or key in ("LD_PRELOAD", "GLIBC_TUNABLES") for key in os.environ):
        parser.error("run with default allocator settings")
    manifest = json.loads((args.fixture / "manifest.json").read_text())
    if manifest["document_count"] != 2014 or tree_digest(args.fixture / "vault") != manifest["tree_sha256"]:
        parser.error("require the unchanged frozen 2014-document fixture")
    args.output.mkdir(parents=True)
    binary = (args.output / "worker").resolve()
    shutil.copy2(receipt["executable"], binary)
    if sha256(binary) != receipt["sha256"]:
        raise ValueError("frozen executable identity changed")
    frozen = args.output / "fixture"
    shutil.copytree(args.fixture, frozen)
    runner = args.output / "runner"
    runner.mkdir()
    for source in Path(__file__).parent.glob("*.py"):
        shutil.copy2(source, runner / source.name)
    metadata = dict(plan=plan, build=receipt, completed=False, source_sha256=source_tree_digest(repo),
                    fixture=manifest, python=sys.version)
    write_json(args.output / "run.json", metadata)
    results = []
    try:
        for run in plan["runs"]:
            root = (args.output / f'r{run["repetition"]}-{run["cell"]}').resolve()
            root.mkdir()
            shutil.copytree(frozen / "vault", root / "vault")
            if tree_digest(root / "vault") != manifest["tree_sha256"]:
                raise ValueError("copied fixture changed")
            result = run_cell(binary, root, manifest, run["cell"]) | run
            results.append(result)
            write_json(root / "result.json", result)
        from loading import whole_get_answer, metadata_answer
        for repetition in range(args.repetitions):
            group = [r for r in results if r["repetition"] == repetition]
            q = next(r for r in group if r["cell"] == "Q")
            reference = {"get": whole_get_answer(next(e for e in q["events"] if e.get("startup_stage") == "canonical-health")),
                         "metadata": {e["probe"]: metadata_answer(e) for e in q["events"] if e.get("startup_stage") == "canonical-final-intake"}}
            for result in group:
                validate_answers(result["cell"], result["events"], manifest, reference)
                for key in ("semantic_sha256", "canonical_identity_sha256", "page_map_sha256", "counts"):
                    if result["store"][key] != results[0]["store"][key]:
                        raise ValueError(f"final ordinary store differs: {key}")
                if result["canonical_work"] is None or result["canonical_work"] != results[0]["canonical_work"]:
                    raise ValueError("ordinary writer work changed across histories")
        if sha256(binary) != receipt["sha256"] or source_tree_digest(repo) != metadata["source_sha256"]:
            raise ValueError("frozen binary or source changed")
        metadata["completed"] = True
    except BaseException as error:
        metadata["failure"] = repr(error)
        raise
    finally:
        write_json(args.output / "run.json", metadata)


if __name__ == "__main__":
    main()
