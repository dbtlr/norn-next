import tempfile
import unittest
import json
import hashlib
import subprocess
from baseline import verify_mutation
from pathlib import Path

from fixture import prepare_probes, tree_digest
from results import distribution, record, validate_paths, validate_write
from provenance import source_tree_digest, verify_build
from report import summarize


class FixtureTests(unittest.TestCase):
    def test_probes_have_known_answers_without_changing_background_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            background = root / "background.md"
            background.write_text("---\ntitle: Background\n---\nExisting body.\n")
            manifest = prepare_probes(root)
            self.assertEqual(manifest["rare_paths"], ["bench/small.md"])
            self.assertEqual(manifest["section"], "## Details\nA bounded section with a precise answer.\n")
            self.assertIn("Existing body.\n", background.read_text())
            self.assertIn("profilecommonword", background.read_text())
            self.assertLess(len(manifest["large_body"].encode()), 65536)
            self.assertEqual(manifest["document_count"], 15)
            self.assertEqual(tree_digest(root), manifest["tree_sha256"])


class AnswerTests(unittest.TestCase):
    def test_a_write_that_loses_unrelated_content_is_rejected(self):
        class Worker:
            def call(self, command):
                return {"answer": {"report": {"document": {
                    "path": "bench/subject.md", "body": {"text": "", "byte_length": 0},
                    "fields": {"status": {"raw": "value0"}}}}}}, 1

        expected = {"bench/subject.md": {"path": "bench/subject.md", "body": "Keep this body.\n",
                                         "fields": {"status": "draft", "perf_bucket": "probe"}}}
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                verify_mutation(Worker(), "current", {"op": "set", "value": "value0"}, Path(directory),
                                {"answer": {"report": {"outcome": "applied", "changeset": "committed"}}}, expected)

    def test_a_short_or_duplicate_answer_cannot_be_a_fast_success(self):
        expected = ["a.md", "b.md", "c.md"]
        validate_paths(["a.md", "b.md"], expected, 2)
        for actual in (["a.md"], ["a.md", "a.md"], ["a.md", "outside.md"]):
            with self.assertRaises(ValueError):
                validate_paths(actual, expected, 2)

    def test_a_complete_answer_must_include_every_matching_document(self):
        with self.assertRaises(ValueError):
            validate_paths(["a.md"], ["a.md", "b.md"], 25)

    def test_truncated_content_and_uncommitted_writes_are_not_successes(self):
        with self.assertRaises(ValueError):
            record("current", {"answer": {"report": {"document": {
                "path": "a.md", "body": {"text": "head", "byte_length": 100}, "fields": {}}}}})
        with self.assertRaises(ValueError):
            validate_write("current", {"answer": {"report": {
                "outcome": "applied", "changeset": "failed"}}})
        with self.assertRaises(ValueError):
            validate_write("legacy", {"structuredContent": {"report": {
                "outcome": "applied", "failed": 1}}})

    def test_summary_preserves_outliers_and_refuses_missing_evidence(self):
        summary = distribution([1_000_000] * 19 + [100_000_000])
        self.assertEqual(summary["median_ms"], 1)
        self.assertEqual(summary["max_ms"], 100)
        self.assertEqual(summary["samples"], 20)
        with self.assertRaises(ValueError):
            distribution([])


class BuildTests(unittest.TestCase):
    def test_an_unchanged_binary_does_not_hide_changed_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "repo"
            repo.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            source = repo / "source.rs"
            source.write_text("original")
            binary = root / "worker"
            binary.write_bytes(b"compiled")
            receipt = root / "build.json"
            receipt.write_text(json.dumps({"executable": str(binary),
                "sha256": hashlib.sha256(b"compiled").hexdigest(),
                "profile": {"opt_level": "3", "debug_assertions": False},
                "source_tree_sha256": source_tree_digest(repo)}))
            verify_build(receipt, repo)
            source.write_text("different")
            with self.assertRaises(ValueError):
                verify_build(receipt, repo)

    def test_changed_binary_cannot_reuse_a_build_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "worker"
            binary.write_bytes(b"changed")
            receipt = root / "build.json"
            receipt.write_text(json.dumps({"executable": str(binary), "sha256": "0" * 64,
                                           "profile": {"opt_level": "3", "debug_assertions": False}}))
            with self.assertRaises(ValueError):
                verify_build(receipt)


class ReportTests(unittest.TestCase):
    def test_an_incomplete_run_cannot_be_reported_as_a_complete_baseline(self):
        with self.assertRaises(ValueError):
            summarize({"completed": False}, [])
        with self.assertRaises(ValueError):
            summarize({"completed": True, "sizes": [300], "samples": 25, "repetitions": 2}, [])


if __name__ == "__main__":
    unittest.main()
