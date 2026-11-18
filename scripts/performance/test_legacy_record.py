"""The stored legacy record: assembled whole from its parts, and never compared once its key fails."""

import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

from legacy_record import SETTINGS, STORED, assemble, key, mismatches, part_records
from nightly import deadline, summary
from record import RECORD_VERSION, resource_rows, summarize

LEGACY = "a" * 64
FIXTURES = {"2000": "b" * 64, "10000": "c" * 64}


def evidence(documents, ready):
    return {"documents": documents, "cold": {"ready": ready, "first_query": ready * 2},
            "warm": {"get_small": [ready, ready + 1_000_000]}, "work": {},
            "resources": {"VmRSS_bytes": 10, "VmHWM_bytes": 20, "open_fds": 5, "threads": 3}}


def part(size, ready, **changes):
    record = {"record_version": RECORD_VERSION, "engine": "legacy", "completed": True,
              "commit": "norn 0.48.3", "build_sha256": LEGACY, "compiler": "unknown",
              "run_at": f"2026-11-18T1{ready % 10}:00:00Z", "finished_at": "2026-11-18T20:00:00Z",
              "load_start": 1.0, "load_end": 1.5, "sizes": [size], "processes": 1, **SETTINGS,
              "fixtures": {str(size): FIXTURES[str(size)]}, "process_evidence": [evidence(size + 14, ready * 1_000_000)]}
    record.update(changes)
    return record


def parts():
    return [(f"legacy-part-{size}-{number}", part(size, number + 1)) for size in (2000, 10000) for number in range(3)]


def stored_record():
    return assemble(parts(), provenance={"run_id": "7", "url": "https://example.invalid/runs/7", "workflow_sha": "d" * 40})


def engine_record(engine="current", sizes=(2000, 10000)):
    rows = [{"documents": size + 14, "metric": "ready", "value": 3.0, "unit": "ms", "status": "measured", "note": ""}
            for size in sizes]
    return {"record_version": RECORD_VERSION, "engine": engine, "commit": "e" * 40, "build_sha256": "f" * 64,
            "compiler": "1.90.0", "run_at": "2026-11-18T02:20:00Z", "load_start": 0.5, "sizes": list(sizes),
            "processes": 3, **SETTINGS, "rows": rows}


class Assembly(unittest.TestCase):
    def test_parts_are_summarized_together_as_one_record_of_three_processes_per_size(self):
        record = assemble(parts())
        processes = [process for _, item in parts() for process in item["process_evidence"]]
        self.assertEqual(record["rows"], summarize(processes) + resource_rows(processes))
        self.assertEqual(record["key"], {"legacy_sha256": LEGACY, "fixtures": FIXTURES, "record_version": RECORD_VERSION})
        self.assertEqual((record["sizes"], record["processes"]), ([2000, 10000], 3))
        ready = {row["documents"]: row["value"] for row in record["rows"] if row["metric"] == "ready"}
        self.assertEqual(ready, {2014: 2.0, 10014: 2.0})
        self.assertNotIn("process_evidence", record)

    def test_a_part_that_does_not_belong_is_refused(self):
        for name, changes in [("incomplete", {"completed": False}),
                              ("version", {"record_version": RECORD_VERSION + 1}),
                              ("reduced", {"samples": 2}),
                              ("executable", {"build_sha256": "9" * 64}),
                              ("fixture", {"fixtures": {"2000": "8" * 64}})]:
            with self.subTest(name):
                items = parts()
                items[1] = (items[1][0], part(2000, 2, **changes))
                with self.assertRaises(ValueError):
                    assemble(items)

    def test_a_size_short_of_its_processes_is_refused(self):
        with self.assertRaisesRegex(ValueError, "2 parts measure 10000"):
            assemble(parts()[:-1])
        with self.assertRaisesRegex(ValueError, "sizes"):
            assemble(parts()[:3])

    def test_parts_are_read_from_each_evidence_archive(self):
        with tempfile.TemporaryDirectory() as scratch:
            for name, item in parts():
                folder = Path(scratch) / name
                folder.mkdir()
                data = json.dumps(item).encode()
                with tarfile.open(folder / "evidence.tar.gz", "w:gz") as tar:
                    info = tarfile.TarInfo("legacy/record.json")
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
            self.assertEqual(assemble(part_records(Path(scratch))), assemble(parts()))


class Key(unittest.TestCase):
    def test_a_record_whose_key_holds_has_no_mismatch(self):
        self.assertEqual(mismatches(stored_record(), key(LEGACY, FIXTURES)), [])

    def test_each_failed_part_of_the_key_is_named(self):
        stored = stored_record()
        reasons = mismatches(stored, key("1" * 64, {"2000": "2" * 64, "300": "3" * 64}))
        self.assertEqual(len(reasons), 3, reasons)
        self.assertIn("legacy executable differs", reasons[0])
        self.assertIn("no 300-document fixture", reasons[1])
        self.assertIn("2000-document fixture differs", reasons[2])
        stored["key"]["record_version"] = RECORD_VERSION + 1
        self.assertIn("record version differs", mismatches(stored, key(LEGACY, FIXTURES))[0])

    def test_a_stored_size_the_run_does_not_measure_is_not_judged(self):
        self.assertEqual(mismatches(stored_record(), key(LEGACY, {"2000": FIXTURES["2000"]})), [])


class Summary(unittest.TestCase):
    def test_no_stored_record_is_said_plainly_and_nothing_is_compared(self):
        text = summary({"current": engine_record()}, None, [], STORED)
        self.assertIn("**No legacy record is stored** at `scripts/performance/legacy-record.json`", text)
        self.assertNotIn("legacy (stored)", text)

    def test_a_stored_record_whose_key_fails_shows_its_reasons_and_no_values(self):
        stored = stored_record()
        text = summary({"serve": engine_record("serve")}, stored, ["the 2000-document fixture differs"])
        self.assertIn("does not match this run, so its values are not shown", text)
        self.assertIn("- the 2000-document fixture differs", text)
        self.assertIn("| legacy (stored) |", text)
        self.assertNotIn("| legacy (stored) | Unit |", text)

    def test_a_matching_stored_record_stands_beside_the_engine_with_its_provenance(self):
        text = summary({"current": engine_record(sizes=(2000,))}, stored_record(), [])
        self.assertIn("matches this run", text)
        self.assertIn("[run 7](https://example.invalid/runs/7)", text)
        self.assertIn("| Documents | Metric | current | legacy (stored) | Unit | Note |", text)
        self.assertIn("| 2014 | ready | 3.0 | 2.0 | ms |", text)
        self.assertNotIn("| 10014 |", text)


class Deadlines(unittest.TestCase):
    def test_a_deadline_grows_with_each_process_and_size(self):
        self.assertEqual(deadline("current", [2000], 2), 2 * deadline("current", [2000], 1))
        self.assertGreater(deadline("legacy", [10000], 1), deadline("legacy", [2000], 1))


if __name__ == "__main__":
    unittest.main()
