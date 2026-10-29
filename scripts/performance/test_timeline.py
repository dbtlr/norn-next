"""The timing table: appended rows, skipped runs, work changes and sustained clock shifts."""

import csv
import json
import tempfile
import unittest
from pathlib import Path

from timeline import COLUMNS, Thresholds, append, compare, read, skip


def record(run_at, commit="aaaa", ready=1000.0, warm=1.0, derived=2014, status="measured"):
    return {"record_version": 1, "completed": True, "run_at": run_at, "commit": commit,
            "build_sha256": "b" * 64, "compiler": "1.97.1", "load_start": 0.5,
            "rows": [
                {"documents": 2014, "metric": "ready", "value": ready, "unit": "ms", "status": status, "note": ""},
                {"documents": 2014, "metric": "warm.get_small.median", "value": warm, "unit": "ms",
                 "status": "measured", "note": ""},
                {"documents": 2014, "metric": "work.documents_derived", "value": derived, "unit": "count",
                 "status": "measured", "note": ""},
            ]}


class Table:
    def __init__(self, test):
        directory = tempfile.TemporaryDirectory()
        test.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.path = self.root / "current.tsv"
        self.serial = 0

    def add(self, **fields):
        self.serial += 1
        source = self.root / f"record-{self.serial}.json"
        source.write_text(json.dumps(record(f"2026-10-{self.serial:02}T04:30:00Z", **fields)))
        append(self.path, source)

    def report(self, **thresholds):
        return compare(read(self.path), Thresholds(**thresholds))


class AppendTests(unittest.TestCase):
    def test_one_row_per_metric_carries_the_run_identity_under_a_single_header(self):
        table = Table(self)
        table.add()
        table.add(commit="bbbb")
        with table.path.open(newline="") as file:
            lines = list(csv.reader(file, delimiter="\t"))
        self.assertEqual(lines[0], list(COLUMNS))
        self.assertEqual(len(lines), 1 + 2 * 3)
        self.assertEqual(lines[1], ["2026-10-01T04:30:00Z", "aaaa", "b" * 64, "1.97.1", "2014", "ready",
                                    "1000.0", "ms", "measured", "0.5", ""])
        self.assertEqual(lines[4][1], "bbbb")

    def test_an_incomplete_record_is_refused_and_appends_nothing(self):
        table = Table(self)
        source = table.root / "partial.json"
        source.write_text(json.dumps({**record("2026-10-01T04:30:00Z"), "completed": False}))
        with self.assertRaises(ValueError):
            append(table.path, source)
        self.assertFalse(table.path.exists())

    def test_a_skipped_run_is_one_row_with_its_reason(self):
        table = Table(self)
        skip(table.path, run_at="2026-10-02T04:30:00Z", commit="aaaa", load=9.5, reason="cargo is running")
        row = read(table.path)[0]
        self.assertEqual((row["metric"], row["status"], row["value"], row["note"], row["load_start"]),
                         ("run", "skipped", "", "cargo is running", "9.5"))


class WorkChangeTests(unittest.TestCase):
    def test_a_counter_change_between_commits_is_reported_once_as_a_work_change(self):
        table = Table(self)
        table.add(commit="aaaa", derived=2014)
        table.add(commit="bbbb", derived=4028)
        report = table.report()
        self.assertEqual(len(report), 1)
        self.assertIn("work change aaaa..bbbb at 2014 documents: work.documents_derived 2014 -> 4028", report[0])
        table.add(commit="bbbb", derived=4028)
        self.assertEqual(table.report(), [])

    def test_a_counter_change_at_the_same_commit_is_reported_as_nondeterministic(self):
        table = Table(self)
        table.add(commit="aaaa", derived=2014)
        table.add(commit="aaaa", derived=2015)
        report = table.report()
        self.assertEqual(len(report), 1)
        self.assertIn("nondeterministic work at aaaa", report[0])

    def test_skipped_runs_do_not_hide_the_previous_commit(self):
        table = Table(self)
        table.add(commit="aaaa", derived=2014)
        skip(table.path, run_at="2026-10-05T04:30:00Z", commit="bbbb", load=9.0, reason="busy")
        table.add(commit="cccc", derived=2016)
        self.assertIn("work change aaaa..cccc", table.report()[0])

    def test_unchanged_counters_across_commits_report_nothing(self):
        table = Table(self)
        table.add(commit="aaaa")
        table.add(commit="bbbb")
        self.assertEqual(table.report(), [])


class ClockShiftTests(unittest.TestCase):
    def history(self, table, values, warm=1.0):
        for value in values:
            table.add(ready=value, warm=warm)

    def test_three_consecutive_runs_past_ratio_and_floor_are_a_sustained_shift(self):
        table = Table(self)
        self.history(table, [1000, 1010, 990, 1000, 1005, 995, 1000, 1300, 1310, 1290])
        report = table.report()
        self.assertEqual(len(report), 1)
        self.assertIn("clock shift at 2014 documents: ready", report[0])
        self.assertIn("1000.000 ms -> 1300.000, 1310.000, 1290.000", report[0])

    def test_two_slow_runs_are_not_yet_sustained(self):
        table = Table(self)
        self.history(table, [1000, 1000, 1000, 1000, 1300, 1300])
        self.assertEqual(table.report(), [])

    def test_a_run_back_under_the_threshold_breaks_the_sequence(self):
        table = Table(self)
        self.history(table, [1000, 1000, 1000, 1300, 1000, 1300, 1300])
        self.assertEqual(table.report(), [])

    def test_a_relative_shift_under_the_absolute_floor_is_not_human_scale(self):
        table = Table(self)
        # Ready doubles from 50 ms, which stays under the 100 ms cold floor.
        self.history(table, [50, 50, 50, 100, 100, 100])
        self.assertEqual(table.report(), [])

    def test_warm_operations_use_their_own_floor(self):
        table = Table(self)
        for warm in [1.0, 1.0, 1.0, 1.6, 1.6, 1.6]:
            table.add(warm=warm)
        self.assertIn("warm.get_small.median", table.report()[0])
        self.assertEqual(table.report(warm_floor_ms=1.0), [])

    def test_the_reference_is_the_median_of_the_runs_before_the_window(self):
        table = Table(self)
        # Eleven earlier runs; only the seven before the recent three form the reference.
        self.history(table, [5000] * 4 + [1000] * 7 + [1300] * 3)
        self.assertIn("1000.000 ms ->", table.report()[0])

    def test_a_short_history_reports_no_shift(self):
        table = Table(self)
        self.history(table, [1000, 1000, 1300, 1300, 1300])
        self.assertEqual(table.report(), [])


class LatestRunTests(unittest.TestCase):
    def test_a_skipped_latest_run_and_an_unavailable_first_read_are_reported(self):
        table = Table(self)
        table.add(status="unavailable")
        report = table.report()
        self.assertIn("unavailable at 2014 documents: ready", report[0])
        skip(table.path, run_at="2026-10-09T04:30:00Z", commit="aaaa", load=9.0, reason="cargo is running")
        self.assertEqual(table.report()[0], "latest run 2026-10-09T04:30:00Z skipped: cargo is running")


if __name__ == "__main__":
    unittest.main()
