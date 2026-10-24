"""Observer histories are exact, acknowledged, and separate from semantic audits."""
import copy
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from residency import CHECKPOINTS, expected_probes, validate_history, validate_ack


def history(cell):
    events = []
    tick = 0
    sequence = 0

    def checkpoint(name):
        nonlocal tick, sequence
        tick += 1
        sequence += 1
        events.append(dict(residency_checkpoint=name, sequence=sequence, construction_eligibility_gets=1, paused_ns=tick,
                           acknowledged_ns=tick + 2,
                           parent_received_ns=tick, parent_sample_started_ns=tick,
                           parent_sample_finished_ns=tick + 1, parent_ack_published_ns=tick + 2, parent_resumed_ns=tick + 3,
                           worker_tid=1, attachment_tid=2, thread_tid=1))
        tick += 3

    def probe(stage, name, command):
        nonlocal tick
        tick += 1
        events.append(dict(startup_stage=stage, probe=name, command=command,
                           query_started_ns=tick, query_finished_ns=tick + 1,
                           thread_tid=1 if cell == "M" and stage != "catalogue" else 2))
        tick += 2

    for stage, name, command in expected_probes(cell):
        if stage == "catalogue":
            probe(stage, name, command)
    for index, name in enumerate(CHECKPOINTS):
        if index == 7:
            from baseline import READS
            for i, (_, command) in enumerate(READS):
                tick += 1
                events.append(dict(diagnostic_common_index=i, command=command, thread_tid=1,
                                   query_started_ns=tick, query_finished_ns=tick + 1))
                tick += 2
        checkpoint(name)
        if index in (0, 2, 4):
            stages = ({"canonical-demand"} if index == 0 else {"canonical-intake"}
                      if index == 2 else {"canonical-health", "canonical-final-intake",
                                             "frontmatter", "lexical", "rich", "health"})
            for stage, probe_name, command in expected_probes(cell):
                if stage in stages:
                    probe(stage, probe_name, command)
    return events


class ResidencyProtocol(unittest.TestCase):
    def test_ambient_diagnostic_option_does_not_change_ordinary_workers(self):
        import os
        from transport import environment
        with TemporaryDirectory() as directory, patch.dict(os.environ, {"NORN_PERFORMANCE_RESIDENCY": "M"}):
            env = environment(Path(directory), "token")
        self.assertNotIn("NORN_PERFORMANCE_RESIDENCY", env)

    def test_diagnostic_mode_removes_the_old_unacknowledged_stage_sample(self):
        from transport import Current
        for cell, expected_samples in ((None, 1), ("N", 0)):
            with TemporaryDirectory() as directory, patch("transport.subprocess.Popen") as process, \
                    patch("transport.threading.Thread"), patch("transport.resources", return_value={}) as sample, \
                    patch.object(Current, "receive", side_effect=[{"startup_stage": "catalogue"}, {"ready": True}]):
                worker = Current(Path("/fake-binary"), Path(directory), residency=cell)
                try:
                    self.assertEqual(sample.call_count, expected_samples)
                    env = process.call_args.kwargs["env"]
                    self.assertEqual(env.get("NORN_PERFORMANCE_RESIDENCY"), cell)
                finally:
                    worker.raw.close()
                    worker.stderr.close()

    def test_paused_parent_samples_then_publishes_ack_before_accepting_continuation(self):
        from residency import PausedCurrent
        with TemporaryDirectory() as directory:
            worker = PausedCurrent.__new__(PausedCurrent)
            worker.run_root = Path(directory)
            (worker.run_root / "residency-acks").mkdir()
            worker.events = []
            worker.process = type("Process", (), {"pid": 1})()
            pause = dict(sequence=1, residency_checkpoint="before-early-get", paused_ns=1,
                         worker_tid=1, attachment_tid=2, thread_tid=1)
            destination = worker.run_root / "residency-acks/1.json"

            def sample(_):
                self.assertFalse(destination.exists())
                return {"thread_ids": [1, 2]}

            def receive():
                if not worker.events and not destination.exists():
                    return pause
                if not worker.events:
                    import json
                    validate_ack(json.loads(destination.read_text()), 1, "before-early-get")
                    return {"residency_acknowledged": 1, "acknowledged_ns": 2}
                return {"ready": True}

            with patch("transport.Current.receive", side_effect=receive), patch("residency.linux_residency", side_effect=sample):
                self.assertEqual(worker.receive(), {"ready": True})
            event = worker.events[0]
            self.assertLessEqual(event["parent_sample_finished_ns"], event["parent_ack_published_ns"])
            self.assertLessEqual(event["parent_ack_published_ns"], event["parent_resumed_ns"])

    def test_full_answers_preserve_collections_rules_and_finding_provenance(self):
        from residency import validate_answers
        from loading import whole_get_answer, metadata_answer
        events = history("M")
        manifest = dict(small_body="Body", document_count=2, active_paths=["active.md"],
                        filtered_paths=["bench/small.md"])
        for event in events:
            if "startup_stage" not in event:
                continue
            event.update(global_ready=False, simulation="paused", projection_epoch="early", projection_generation=3)
            stage, probe = event["startup_stage"], event["probe"]
            if stage == "catalogue":
                event["candidates"] = [{"target": "small", "paths": ["bench/small.md"], "total": 1},
                    {"target": "roadmap", "paths": [str(i) for i in range(5)], "total": 6},
                    {"target": "profilenonexistentdocument", "paths": [], "total": 0}]
                continue
            if stage == "canonical-demand":
                document = dict(path="bench/small.md", body={"text": "Body", "byte_length": 4},
                                fields={"status": {"raw": "draft"}, "perf_bucket": {"raw": "probe"}})
                document.update({k: {"items": [], "total": 0} for k in ("headings", "blocks", "tags", "links")})
                document["findings"] = {"items": [{"id": 7, "generation": 3, "kind": "broken_link", "target": "missing"}], "total": 1}
                report = dict(shape="record", document=document, rule_sets=["schema-pin"])
                unsatisfied = []
            elif event["command"]["op"] == "count":
                report = dict(rows=[{"group": [], "count": 1 if probe == "active_count" else 2}], next=None,
                              counted_at={"epoch": "early", "generation": 3}, rule_sets=["schema-pin"])
                unsatisfied = []
            else:
                paths = (["active.md"] if probe == "active_page" else ["bench/small.md"]
                         if probe in ("sparse_page", "field_page") else [])
                rows = [{"path": path} for path in paths]
                if probe == "field_page":
                    rows[0]["fields"] = {"perf_bucket": {"raw": "probe"}}
                report = dict(page={"rows": rows, "next": None, "moved": []}, rule_sets=["schema-pin"])
                unsatisfied = ([{"part": "unknown_predicate_key", "key": "profilenonexistentkey"}]
                               if probe == "unknown_field" else [])
            event["answer"] = dict(report=report, unsatisfied=unsatisfied, advisories=[])
        early = next(e for e in events if e.get("startup_stage") == "canonical-demand")
        reference = dict(get=whole_get_answer(early), metadata={e["probe"]: metadata_answer(e) for e in events
                         if e.get("startup_stage") == "canonical-intake"})
        validate_answers("M", events, manifest, reference)
        for mutation in ("collection", "finding", "provenance", "rules", "sparse", "zero", "unknown", "field"):
            changed = copy.deepcopy(events)
            get = next(e for e in changed if e.get("startup_stage") == "canonical-demand")
            metadata = {e["probe"]: e for e in changed if e.get("startup_stage") == "canonical-intake"}
            if mutation == "collection":
                get["answer"]["report"]["document"]["links"] = {"items": [{"target": "lost"}], "total": 1}
            elif mutation == "finding":
                get["answer"]["report"]["document"]["findings"]["items"][0]["target"] = "changed"
            elif mutation == "provenance":
                get["answer"]["report"]["document"]["findings"]["items"][0]["generation"] = 4
            elif mutation == "rules":
                metadata["active_page"]["answer"]["report"]["rule_sets"] = ["changed"]
            elif mutation == "sparse":
                metadata["sparse_page"]["answer"]["report"]["page"]["rows"] = []
            elif mutation == "zero":
                metadata["zero_page"]["answer"]["report"]["page"]["rows"] = [{"path": "extra.md"}]
            elif mutation == "unknown":
                metadata["unknown_field"]["answer"]["unsatisfied"] = []
            else:
                metadata["field_page"]["answer"]["report"]["page"]["rows"][0]["fields"] = {}
            with self.assertRaises(ValueError, msg=mutation):
                validate_answers("M", changed, manifest, reference)

    def test_mapping_census_distinguishes_file_rss_from_anonymous_pages(self):
        from residency import linux_residency
        with TemporaryDirectory() as directory:
            root = Path(directory)
            proc = root / "42"
            (proc / "task" / "42").mkdir(parents=True)
            (proc / "task" / "43").mkdir()
            (proc / "smaps").write_text("1000-2000 r--p 00000000 08:01 1 /binary\nRss: 8 kB\nAnonymous: 4 kB\n"
                "2000-3000 rw-p 00000000 00:00 0 [heap]\nRss: 12 kB\nAnonymous: 12 kB\n"
                "3000-4000 r-xp 00000000 00:00 0 [vdso]\nRss: 4 kB\nAnonymous: 0 kB\n")
            (proc / "status").write_text("RssFile: 4 kB\nRssAnon: 16 kB\nRssShmem: 0 kB\n")
            census = linux_residency(42, root)
        self.assertEqual(census["file_mapping_rss_bytes"], 8 * 1024)
        self.assertEqual(census["anonymous_pages_bytes"], 16 * 1024)
        self.assertEqual(census["anonymous_mapping_rss_bytes"], 12 * 1024)
        self.assertEqual(census["mapping_rss_bytes"], 24 * 1024)
        self.assertEqual(census["thread_ids"], [42, 43])

    def test_five_exact_histories_and_thread_placement(self):
        for cell, count in [("N", 1), ("G", 2), ("E", 9), ("Q", 27), ("M", 9)]:
            events = history(cell)
            self.assertEqual(len(expected_probes(cell)), count)
            validate_history(cell, events)
            query = next(e for e in events if "startup_stage" in e)
            for mutation in ("missing", "duplicate", "unexpected", "command", "thread"):
                changed = copy.deepcopy(events)
                selected = next(e for e in changed if "startup_stage" in e)
                if mutation == "missing":
                    changed.remove(selected)
                elif mutation == "duplicate":
                    changed.insert(0, copy.deepcopy(query))
                elif mutation == "unexpected":
                    selected["probe"] = "invented"
                elif mutation == "command":
                    selected["command"] = {"op": "count"}
                else:
                    selected["thread_tid"] = 999
                with self.assertRaises(ValueError, msg=(cell, mutation)):
                    validate_history(cell, changed)

    def test_sampling_acknowledgement_and_query_order_are_required(self):
        for mutation in ("sampling", "sequence", "early", "missing", "duplicate"):
            events = history("E")
            first = next(e for e in events if "residency_checkpoint" in e)
            if mutation == "sampling":
                first["parent_sample_finished_ns"] = first["parent_ack_published_ns"] + 1
            elif mutation == "sequence":
                first["sequence"] = 8
            elif mutation == "early":
                query = next(e for e in events if e.get("startup_stage") == "canonical-demand")
                query["query_started_ns"] = first["paused_ns"]
            elif mutation == "missing":
                events.remove(first)
            else:
                events.append(copy.deepcopy(first))
            with self.assertRaises(ValueError, msg=mutation):
                validate_history("E", events)

    def test_ack_is_bound_to_the_paused_checkpoint(self):
        validate_ack({"sequence": 1, "checkpoint": "before-early-get"}, 1, "before-early-get")
        for ack in ({}, {"sequence": True, "checkpoint": "before-early-get"},
                    {"sequence": 2, "checkpoint": "before-early-get"},
                    {"sequence": 1, "checkpoint": "after-early-get"}):
            with self.assertRaises(ValueError):
                validate_ack(ack, 1, "before-early-get")

    def test_common_reads_follow_ready_and_finish_before_last_endpoint(self):
        for mutation in ("missing", "duplicate", "command", "thread", "early", "late"):
            events = history("E")
            read = next(e for e in events if "diagnostic_common_index" in e)
            if mutation == "missing":
                events.remove(read)
            elif mutation == "duplicate":
                events.append(copy.deepcopy(read))
            elif mutation == "command":
                read["command"] = {"op": "get", "target": "extra"}
            elif mutation == "thread":
                read["thread_tid"] = 2
            elif mutation == "early":
                read["query_started_ns"] = 0
            else:
                read["query_finished_ns"] = events[-1]["paused_ns"] + 1
            with self.assertRaises(ValueError, msg=mutation):
                validate_history("E", events)

    def test_final_endpoint_precedes_semantic_validation(self):
        from residency import run_cell
        from baseline import READS

        class Worker:
            process = type("Process", (), {"pid": 1})()
            events = history("N")
            canonical_work = {}
            ready = {}
            endpoint = False
            closed = False

            def call(self, command):
                if command == {"op": "residency-endpoint"}:
                    self.endpoint = True
                    return {"answer": {"residency_endpoint": True}}, 0
                index = [c for _, c in READS].index(command)
                return {"thread_tid": 1, "diagnostic_common_index": index,
                        "query_started_ns": 1, "query_finished_ns": 2}, 0

            def close(self):
                self.closed = True

        worker = Worker()
        def validate(*_):
            self.assertTrue(worker.endpoint)
            raise ValueError("stop after endpoint")
        with patch("residency.PausedCurrent", return_value=worker), patch("residency.validate_history"), \
                patch("results.validate_read", side_effect=validate):
            with self.assertRaisesRegex(ValueError, "stop after endpoint"):
                run_cell(Path("/binary"), Path("/isolated"), {}, "N")
        self.assertTrue(worker.closed)

    def test_plan_is_serial_and_reverses_the_same_five_histories(self):
        from residency import execution_plan
        plan = execution_plan()
        self.assertTrue(plan["serial"])
        self.assertEqual(plan["frozen_fixture_document_count"], 2014)
        self.assertEqual([r["cell"] for r in plan["runs"]], list("NGEQMMQEGN"))
        with self.assertRaises(ValueError):
            execution_plan(1)


if __name__ == "__main__":
    unittest.main()
