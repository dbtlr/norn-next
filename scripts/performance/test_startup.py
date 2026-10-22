import copy
import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from provenance import cargo_configs, source_tree_digest
from startup import baseline_build, partition, storage_bytes, validate_work
from startup_report import render, validate_samples


class StartupAccountingTests(unittest.TestCase):
    def test_report_keeps_measurements_when_optional_build_identity_was_not_recorded(self):
        build = {"sha256": "binary-hash", "source_tree_sha256": "source-hash"}
        metadata = {"builds": {"baseline": build}, "started_at": "2026-10-22", "repetitions": 1,
                    "platform": "test platform", "load_start": [0, 0, 0], "load_end": [0, 0, 0]}
        row = {"document_count": 314, "changeset_size": 128, "build_kind": "baseline",
               "profiling": False, "stage": "fresh", "shutdown": None, "validated_lifecycle": None,
               "first_validated_query": {"median_ms": 1234}, "observed_ready": {"median_ms": 1000},
               "repetition_seconds": [1.234], "cpu_seconds": 0.9, "peak_mib": 20, "work": None}
        recorded = {"repository_head": "revision<one>", "rustc": "rustc <version>\nmore details",
                    "cargo_lock_sha256": "lock-hash"}
        for fields in ({}, dict.fromkeys(recorded), dict.fromkeys(recorded, ""), recorded):
            with self.subTest(fields=fields):
                build.clear()
                build.update(sha256="binary-hash", source_tree_sha256="source-hash", **fields)
                page = render(metadata, [row])
                self.assertIn("<td>1.234</td>", page)
                self.assertIn("<code>binary-hash</code>", page)
                self.assertIn("<code>source-hash</code>", page)
                if fields == recorded:
                    self.assertIn("revision&lt;one&gt;", page)
                    self.assertIn("rustc &lt;version&gt;", page)
                    self.assertIn("lock-hash", page)
                    self.assertNotIn("more details", page)
                else:
                    self.assertEqual(page.count("not recorded"), 5)

    def test_storage_readings_include_the_open_wal_without_reading_database_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data = root / "data/private-store"
            data.mkdir(parents=True)
            (data / "store.sqlite3").write_bytes(b"database")
            (data / "store.sqlite3-wal").write_bytes(b"wal")
            self.assertEqual(storage_bytes(root), {"database": 8, "wal": 3, "shared_memory": 0})
            (data / "store.sqlite3-wal").unlink()
            self.assertEqual(storage_bytes(root)["wal"], 0)

    def test_revision_comparison_verifies_the_baselines_own_source_and_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "baseline-source"
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            (source / "worker.rs").write_text("baseline", encoding="utf-8")
            binary = root / "baseline-worker"
            binary.write_bytes(b"compiled baseline")
            receipt = root / "baseline.json"
            built = {"executable": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                     "profile": {"opt_level": "3", "debug_assertions": False}, "features": [],
                     "cargo_configs": cargo_configs(source), "source_tree_sha256": source_tree_digest(source)}
            receipt.write_text(json.dumps(built), encoding="utf-8")
            self.assertEqual(baseline_build(receipt, source), built)
            (source / "worker.rs").write_text("changed baseline", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "source tree"):
                baseline_build(receipt, source)
            (source / "worker.rs").write_text("baseline", encoding="utf-8")
            binary.write_bytes(b"different executable")
            with self.assertRaisesRegex(ValueError, "executable"):
                baseline_build(receipt, source)
            binary.write_bytes(b"compiled baseline")
            built["features"] = ["induced-failure"]
            receipt.write_text(json.dumps(built), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "no-feature"):
                baseline_build(receipt, source)

    def ready(self):
        return {"startup_complete_ns": 30, "timings": {
            "unknown_labels": 0, "unfinished_spans": 0,
            "entries": [
                {"label": "host.document_plan", "calls": 2, "elapsed_ns": 7},
                {"label": "store.increment", "calls": 1, "elapsed_ns": 12},
                {"label": "store.upsert", "calls": 2, "elapsed_ns": 4},
                {"label": "store.link_health", "calls": 1, "elapsed_ns": 5},
                {"label": "store.commit", "calls": 1, "elapsed_ns": 1},
            ]}}

    def test_parent_and_child_times_are_not_added_twice(self):
        self.assertEqual(partition(self.ready()), {
            "document_plan": 7, "row_and_index_writes": 4, "link_health": 5,
            "commit": 1, "other_increment": 2, "other_startup": 11})

    def test_incomplete_or_overlapping_timings_cannot_be_presented_as_a_breakdown(self):
        for field in ("unknown_labels", "unfinished_spans"):
            ready = self.ready()
            ready["timings"][field] = 1
            with self.assertRaises(ValueError):
                partition(ready)
        ready = self.ready()
        ready["timings"]["entries"][2]["elapsed_ns"] = 20
        with self.assertRaises(ValueError):
            partition(ready)
        ready = self.ready()
        ready["timings"]["entries"].append(copy.deepcopy(ready["timings"]["entries"][0]))
        with self.assertRaises(ValueError):
            partition(ready)

    def test_disabled_timing_is_not_reported_as_zero_cost(self):
        self.assertIsNone(partition({"startup_complete_ns": 30, "timings": None}))

    def test_report_requires_each_requested_fresh_and_restart_sample_once(self):
        metadata = {"completed": True, "sizes": [300], "batches": [128],
                    "variants": [{"build_kind": "profile", "profiling": True}], "repetitions": 1}
        common = {"repetition": 0, "background_documents": 300,
                  "changeset_size": 128, "build_kind": "profile", "profiling": True, "verified": True}
        fresh, restart = ({**common, "stage": stage} for stage in ("fresh", "restart"))
        validate_samples(metadata, [fresh, restart])
        for samples in ([fresh], [fresh, fresh], [fresh, restart, restart]):
            with self.assertRaises(ValueError):
                validate_samples(metadata, samples)
        with self.assertRaises(ValueError):
            validate_samples({**metadata, "completed": False}, [fresh, restart])

    def test_work_evidence_rejects_incomplete_import_and_a_rebuilt_restart(self):
        with self.assertRaises(ValueError):
            validate_work({"changesets_applied": 2}, "fresh", 314, 128)
        validate_work({"changesets_applied": 3}, "fresh", 314, 128)
        unchanged = {"recoveries_run": 0, "rebuilds_run": 0, "documents_derived": 0, "documents_upserted": 0}
        validate_work(unchanged, "restart", 314, 128)
        for key in unchanged:
            with self.assertRaises(ValueError):
                validate_work({**unchanged, key: 1}, "restart", 314, 128)

    def test_revision_reports_require_shutdown_and_the_complete_validated_lifecycle(self):
        metadata = {"completed": True, "workload_version": "startup-attribution-v2", "sizes": [300],
                    "batches": [128], "variants": [{"build_kind": "standard", "profiling": False}], "repetitions": 1}
        common = {"repetition": 0, "background_documents": 300, "changeset_size": 128,
                  "build_kind": "standard", "profiling": False, "verified": True,
                  "first_validated_query_ns": 20, "shutdown_ns": 30, "validated_lifecycle_ns": 60}
        samples = [{**common, "stage": stage} for stage in ("fresh", "restart")]
        validate_samples(metadata, samples)
        for key in ("shutdown_ns", "validated_lifecycle_ns"):
            incomplete = copy.deepcopy(samples)
            del incomplete[0][key]
            with self.assertRaises(ValueError):
                validate_samples(metadata, incomplete)
        for shutdown, lifecycle in ((-1, 60), (30, 49)):
            inconsistent = copy.deepcopy(samples)
            inconsistent[0].update(shutdown_ns=shutdown, validated_lifecycle_ns=lifecycle)
            with self.assertRaises(ValueError):
                validate_samples(metadata, inconsistent)


if __name__ == "__main__":
    unittest.main()
