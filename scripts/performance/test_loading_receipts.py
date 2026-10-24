"""Execution receipts must prove the completed private health-once build."""
import copy
import unittest
import io
import tempfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import loading


def receipts():
    work = dict(documents_derived=3, documents_upserted=3, documents_deleted=0,
                tombstones_recorded=0, changesets_applied=2, findings_written=1,
                findings_discarded=0, links_redecided=0, link_health_keys_resolved=0,
                link_health_candidates_read=0, changeset_read_steps=2)
    stages = []
    for final, boundary in [(False, 10), (True, 20)]:
        current = dict(work)
        if final:
            current.update(links_redecided=4, link_health_keys_resolved=2,
                           link_health_candidates_read=3, findings_written=2, changeset_read_steps=8)
        stages.append(dict(startup_stage='health-once-health' if final else 'health-once-facts',
            probe='boundary', loading_mode='health-once', mode_executed=True,
            simulation='private-initial-build', global_ready=False, queries_executed=False,
            query_host_ns=0, health_complete=final, stage_boundary_ns=boundary, health_once_work=current))
    ready = dict(ready=True, startup_complete_ns=30, changeset_size=2,
                 job_evidence=dict(stages[-1]['health_once_work'], document_opens=3, stats=5,
                                   walk_dirents=3, recoveries_run=0, rebuilds_run=0))
    return stages, ready, dict(document_count=3)


class ReceiptChecks(unittest.TestCase):
    def check(self, stages, ready, manifest, links=4):
        loading.validate_receipts('health-once', stages, ready, manifest, stored_links=links)

    def test_missing_malformed_wrong_mode_and_reordered_receipts_are_rejected(self):
        stages, ready, manifest = receipts()
        self.check(stages, ready, manifest)
        for key in stages[0]:
            changed = copy.deepcopy(stages)
            del changed[0][key]
            with self.subTest(missing=key), self.assertRaises(ValueError):
                self.check(changed, ready, manifest)
        for key, bad in [('loading_mode', 'ordinary'), ('mode_executed', 1),
                ('simulation', 'published'), ('global_ready', True), ('queries_executed', True),
                ('health_complete', True), ('stage_boundary_ns', -1), ('stage_boundary_ns', True),
                ('query_host_ns', 1), ('query_host_ns', False)]:
            changed = copy.deepcopy(stages)
            changed[0][key] = bad
            with self.subTest(key=key, bad=bad), self.assertRaises(ValueError):
                self.check(changed, ready, manifest)
        for changed in [[], stages[:1], stages[::-1], stages + stages[:1]]:
            with self.subTest(order=changed), self.assertRaises(ValueError):
                self.check(changed, ready, manifest)
        for boundary in [10, 30, 31, float('inf')]:
            changed = copy.deepcopy(stages)
            changed[1]['stage_boundary_ns'] = boundary
            with self.subTest(boundary=boundary), self.assertRaises(ValueError):
                self.check(changed, ready, manifest)

    def test_work_conservation_ready_account_and_exact_link_judgments_are_required(self):
        stages, ready, manifest = receipts()
        for index, key, bad in [(0, 'links_redecided', 1), (0, 'link_health_keys_resolved', 1),
                (0, 'documents_upserted', 2), (1, 'changesets_applied', 3),
                (1, 'documents_deleted', 1), (1, 'findings_written', 0),
                (1, 'links_redecided', 3), (1, 'changeset_read_steps', True),
                (1, 'changeset_read_steps', -1)]:
            changed = copy.deepcopy(stages)
            changed[index]['health_once_work'][key] = bad
            with self.subTest(index=index, key=key, bad=bad), self.assertRaises(ValueError):
                self.check(changed, ready, manifest)
        for key in stages[1]['health_once_work']:
            changed = copy.deepcopy(ready)
            changed['job_evidence'][key] += 1
            with self.subTest(account=key), self.assertRaises(ValueError):
                self.check(stages, changed, manifest)
        for key in ['startup_complete_ns', 'job_evidence', 'changeset_size']:
            changed = copy.deepcopy(ready)
            del changed[key]
            with self.subTest(ready=key), self.assertRaises(ValueError):
                self.check(stages, changed, manifest)

    def test_ready_clocks_and_every_supplied_work_reading_are_nonnegative_integers(self):
        stages, ready, manifest = receipts()
        for key, value in [("startup_complete_ns", True), ("attach_ns", -1),
                           ("observed_ready_ns", 31), ("host_setup_ns", False)]:
            changed = copy.deepcopy(ready)
            changed[key] = value
            with self.subTest(clock=key), self.assertRaises(ValueError):
                self.check(stages, changed, manifest)
        for key in ["watcher_polls", "document_opens", "stats", "walk_dirents"]:
            changed = copy.deepcopy(ready)
            changed["job_evidence"][key] = True
            with self.subTest(work=key), self.assertRaises(ValueError):
                self.check(stages, changed, manifest)

    def test_cold_controller_rejects_missing_receipts_before_any_query(self):
        stages, ready, manifest = receipts()
        stages[1].pop("health_complete")
        calls = []
        worker = SimpleNamespace(stages=stages, ready=ready,
            call=lambda command: calls.append(command), close=lambda: None)
        with tempfile.TemporaryDirectory() as directory, patch("loading.Current", return_value=worker):
            with self.assertRaisesRegex(ValueError, "health_complete"):
                loading.run_one(Path("unused"), Path(directory), manifest, "health-once", profiling=False)
        self.assertEqual(calls, [])

    def test_cold_comparison_rejects_changed_identity_work_or_complete_answer(self):
        _, ready, _ = receipts()
        result = dict(store=dict(schema_version=3, source_sha256="source", semantic_sha256="logical", document_construction_sha256="identities"),
                      ready=ready, first_get=dict(normalized_answer={"all": "columns"}))
        reference = loading.cold_equivalence(result)
        for key in ["semantic_sha256", "document_construction_sha256"]:
            changed = copy.deepcopy(result)
            changed["store"][key] = "changed"
            with self.subTest(key=key), self.assertRaises(ValueError):
                loading.compare_cold_arms(reference, changed)
        for key in loading.ORDINARY_WORK:
            changed = copy.deepcopy(result)
            changed["ready"]["job_evidence"][key] += 1
            with self.subTest(work=key), self.assertRaises(ValueError):
                loading.compare_cold_arms(reference, changed)
        changed = copy.deepcopy(result)
        changed["first_get"]["normalized_answer"]["extra"] = "retained"
        with self.assertRaises(ValueError):
            loading.compare_cold_arms(reference, changed)
        changed = copy.deepcopy(result)
        changed["store"].update(canonical_identity_sha256="different", page_map_sha256="different",
                                 write_generation=1000, store_epoch="different")
        loading.compare_cold_arms(reference, changed)

    def test_warm_worker_rejects_invalid_receipts_before_any_main_thread_query(self):
        from baseline import run_worker
        from fixture import tree_digest
        stages, ready, manifest = receipts()
        stages[0].pop("loading_mode")
        calls = []
        worker = SimpleNamespace(stages=stages, ready=ready,
            call=lambda command: calls.append(command), close=lambda: None)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "fixture"
            (fixture / "vault").mkdir(parents=True)
            manifest.update(tree_sha256=tree_digest(fixture / "vault"), probe_bodies={})
            args = SimpleNamespace(output=root, current="unused", loading="health-once")
            with patch("baseline.Current", return_value=worker):
                with self.assertRaisesRegex(ValueError, "loading_mode"):
                    run_worker(args, "current", 0, 0, fixture, manifest, io.StringIO())
        self.assertEqual(calls, [])


if __name__ == '__main__':
    unittest.main()
