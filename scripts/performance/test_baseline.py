import tempfile
import unittest
import json
import hashlib
import subprocess
import os
import signal
import sys
import time
from baseline import verify_mutation
from pathlib import Path

from fixture import prepare_probes, tree_digest
from results import distribution, record, validate_paths, validate_write
from provenance import cargo_configs, run_build, source_tree_digest, verify_build
from report import summarize
from transport import stop_process


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
    def test_a_successful_cached_read_does_not_prove_the_field_was_written(self):
        before = b"---\nstatus: draft\nperf_bucket: probe\n---\nKeep the body.\n"
        state = {"bench/subject.md": {"path": "bench/subject.md", "body": "Keep the body.\n",
                                     "fields": {"status": "draft", "perf_bucket": "probe"}}}
        class Worker:
            def call(self, _command):
                return {"structuredContent": {"records": [{"path": "bench/subject.md", "body": "Keep the body.\n",
                    "frontmatter": {"status": "value0", "perf_bucket": "probe"}}]}}, 1
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "vault/bench").mkdir(parents=True)
            (root / "vault/bench/subject.md").write_bytes(before)
            with self.assertRaises(ValueError):
                verify_mutation(Worker(), "legacy", {"op": "set", "value": "value0"}, root,
                    {"structuredContent": {"report": {"outcome": "applied"}}}, state, before)

    def test_a_confirming_get_cannot_hide_an_unexpected_advisory(self):
        with self.assertRaises(ValueError):
            record("current", {"answer": {"advisories": [{"unexpected": "diagnostic"}],
                "report": {"document": {"path": "a.md", "body": {"text": "", "byte_length": 0}, "fields": {}}}}})

    def test_exact_create_bytes_are_not_newline_normalized(self):
        command = {"op": "new", "path": "new.md", "body": "# New\n",
                   "content": "---\nstatus: draft\n---\n# New\n"}
        class Worker:
            def call(self, _command):
                return {"structuredContent": {"records": [{"path": "new.md", "body": "# New\n",
                    "frontmatter": {"status": "draft"}}]}}, 1
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "vault").mkdir()
            (root / "vault/new.md").write_bytes(command["content"].replace("\n", "\r\n").encode())
            with self.assertRaises(ValueError):
                verify_mutation(Worker(), "legacy", command, root,
                    {"structuredContent": {"report": {"outcome": "applied"}}}, {})

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
    def test_a_timed_out_build_preserves_its_partial_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            command = [sys.executable, "-c", "import sys,time; print('{}',flush=True); print('diagnostic',file=sys.stderr,flush=True); time.sleep(60)"]
            with self.assertRaises(subprocess.TimeoutExpired):
                run_build(command, root, os.environ.copy(), root / "build.json", timeout=2)
            self.assertIn("diagnostic", (root / "build.cargo.log").read_text())
            self.assertIn("{}", (root / "build.cargo.jsonl").read_text())

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
                "cargo_configs": cargo_configs(repo),
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


@unittest.skipUnless(sys.platform == "linux", "Linux process-group lifecycle")
class ProcessTests(unittest.TestCase):
    def test_cleanup_stops_descendants_after_the_leader_exits(self):
        code = "import os,signal,time\npid=os.fork()\nif pid==0:\n signal.signal(signal.SIGTERM,signal.SIG_IGN)\n print(os.getpid(),flush=True)\n time.sleep(60)\nelse:\n time.sleep(0.1)\n"
        process = subprocess.Popen([sys.executable, "-c", code], stdout=subprocess.PIPE,
                                   text=True, start_new_session=True)
        try:
            child = int(process.stdout.readline())
            process.wait(timeout=5)
            stop_process(process)
            until = time.monotonic() + 2
            state = ""
            while time.monotonic() < until:
                try:
                    state = (Path("/proc") / str(child) / "stat").read_text().rsplit(")", 1)[1].split()[0]
                except FileNotFoundError:
                    break
                if state == "Z":
                    break
                time.sleep(0.01)
            else:
                self.fail(f"descendant still runs after cleanup: {state}")
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
            process.stdout.close()


if __name__ == "__main__":
    unittest.main()
