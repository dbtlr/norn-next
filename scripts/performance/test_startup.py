import copy
import unittest

from startup import partition, validate_work
from startup_report import validate_samples


class StartupAccountingTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
