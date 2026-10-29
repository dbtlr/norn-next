"""The record mode's flat metric rows and its first-read acknowledgement."""

import json
import tempfile
import unittest
from pathlib import Path

from record import acknowledge, measured, summarize


def process(documents=2014, first_get=400_000_000, first_find=900_000_000, ready=3_000_000_000,
            warm=None, work=None):
    return {"documents": documents,
            "cold": {"first_get": first_get, "first_find": first_find, "ready": ready},
            "warm": warm if warm is not None else {"get_small": [1_000_000, 2_000_000, 3_000_000]},
            "work": work if work is not None else {"documents_derived": 2014, "bytes_written": 81920}}


def rows_by_metric(rows):
    return {(row["documents"], row["metric"]): row for row in rows}


class SummaryTests(unittest.TestCase):
    def test_each_size_reports_cold_clocks_warm_distributions_and_counters_as_flat_rows(self):
        rows = summarize([process(), process(), process()])
        for row in rows:
            self.assertEqual(set(row), {"documents", "metric", "value", "unit", "status", "note"})
        metrics = rows_by_metric(rows)
        self.assertEqual(sorted(metric for _, metric in metrics), sorted([
            "first_get", "first_find", "ready", "warm.get_small.median", "warm.get_small.p95",
            "work.documents_derived", "work.bytes_written"]))
        self.assertEqual(metrics[(2014, "ready")], {"documents": 2014, "metric": "ready", "value": 3000.0,
                                                    "unit": "ms", "status": "measured", "note": ""})
        self.assertEqual(metrics[(2014, "work.bytes_written")]["unit"], "bytes")
        self.assertEqual(metrics[(2014, "work.documents_derived")]["unit"], "count")

    def test_cold_clocks_and_warm_distributions_take_the_median_across_processes(self):
        rows = rows_by_metric(summarize([
            process(ready=1_000_000_000, warm={"get_small": [1_000_000] * 9 + [9_000_000]}),
            process(ready=5_000_000_000, warm={"get_small": [2_000_000] * 9 + [8_000_000]}),
            process(ready=2_000_000_000, warm={"get_small": [3_000_000] * 9 + [7_000_000]}),
        ]))
        self.assertEqual(rows[(2014, "ready")]["value"], 2000.0)
        # Each process's median is 1, 2 and 3 ms; each p95 is that process's slowest of ten calls.
        self.assertEqual(rows[(2014, "warm.get_small.median")]["value"], 2.0)
        self.assertEqual(rows[(2014, "warm.get_small.p95")]["value"], 8.0)

    def test_sizes_are_reported_separately(self):
        rows = rows_by_metric(summarize([process(documents=2014, ready=1_000_000_000),
                                         process(documents=10014, ready=9_000_000_000)]))
        self.assertEqual(rows[(2014, "ready")]["value"], 1000.0)
        self.assertEqual(rows[(10014, "ready")]["value"], 9000.0)

    def test_a_counter_that_differs_between_processes_is_unstable_not_a_measurement(self):
        rows = rows_by_metric(summarize([
            process(work={"bytes_written": 100}), process(work={"bytes_written": 300}),
            process(work={"bytes_written": 200})]))
        row = rows[(2014, "work.bytes_written")]
        self.assertEqual((row["status"], row["value"]), ("unstable", 200))
        self.assertEqual(row["note"], "processes disagree: 100, 300, 200")

    def test_a_refused_first_read_is_unavailable_and_carries_no_value(self):
        rows = rows_by_metric(summarize([process(first_get=None), process(), process()]))
        row = rows[(2014, "first_get")]
        self.assertEqual((row["status"], row["value"]), ("unavailable", None))
        self.assertEqual(row["note"], "1 of 3 processes refused the first read")
        self.assertEqual(rows[(2014, "first_find")]["status"], "measured")

    def test_an_engine_without_first_reads_reports_only_its_own_cold_clocks(self):
        legacy = {"documents": 2014, "cold": {"ready": 2_000_000_000, "first_query": 2_500_000_000},
                  "warm": {"get_small": [1_000_000]}, "work": {}}
        metrics = {metric for _, metric in rows_by_metric(summarize([legacy]))}
        self.assertEqual(metrics, {"ready", "first_query", "warm.get_small.median", "warm.get_small.p95"})


class SampleSelectionTests(unittest.TestCase):
    def test_reads_drop_their_warmups(self):
        self.assertEqual(measured("get_small", list(range(10)), warmups=3, write_payoff=200), list(range(3, 10)))

    def test_writes_also_drop_the_post_build_payoff_window(self):
        timings = list(range(405))
        self.assertEqual(measured("rename_hub", timings, warmups=5, write_payoff=200), list(range(200, 405)))
        self.assertEqual(measured("create", timings, warmups=300, write_payoff=200), list(range(300, 405)))

    def test_a_scenario_with_no_measured_call_is_refused(self):
        with self.assertRaises(ValueError):
            measured("set_field", list(range(200)), warmups=5, write_payoff=200)


class AcknowledgementTests(unittest.TestCase):
    def test_each_first_read_is_acknowledged_with_its_sequence_and_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            handler = acknowledge(root)
            handler({"operator_prefix_sequence": 1, "probe": "whole_slug"})
            handler({"operator_prefix_sequence": 2, "probe": "active_page"})
            acks = root / "operator-prefix-acks"
            self.assertEqual(json.loads((acks / "1.json").read_text()), {"sequence": 1, "probe": "whole_slug"})
            self.assertEqual(json.loads((acks / "2.json").read_text()), {"sequence": 2, "probe": "active_page"})
            self.assertEqual(sorted(path.name for path in acks.iterdir()), ["1.json", "2.json"])


if __name__ == "__main__":
    unittest.main()
