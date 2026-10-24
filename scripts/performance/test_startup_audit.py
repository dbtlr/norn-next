"""Source validation at the audit-worker protocol, without opening a database."""

import copy
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from fixture import tree_digest
from startup_audit import audit_store
from transport import PREFIX, issue_root


def fixture(root, mutate=None, exit_code=0):
    vault = root / "vault"
    vault.mkdir(parents=True)
    bodies = {"a.md": "café\n", "b.md": "beta\n"}
    records = []
    for name, body in bodies.items():
        data = body.encode("utf-8")
        (vault / name).write_bytes(data)
        records.append({"document": {"path": name, "content_hash": hashlib.sha256(data).hexdigest(),
                                     "byte_length": len(data)}})
    manifest = {"paths": sorted(bodies), "document_count": 2, "markdown_bytes": 11,
                "tree_sha256": tree_digest(vault)}
    summary = {"semantic_sha256": "ab" * 32, "tables": {"documents": {"rows": 2, "sha256": "cd" * 32}},
               "counts": {"documents": 2}, "store_epoch": "epoch", "write_generation": 3,
               "schema_version": 3, "document_construction_sha256": "ef" * 32}
    records.append({"summary": summary})
    if mutate:
        mutate(records)
    data = root / "data"
    data.mkdir()
    (data / "store.sqlite3").write_bytes(b"inert test marker; no test opens this file")
    issue_root(root)
    worker = root / "worker"
    worker.write_text(
        f"#!{sys.executable}\n"
        "import json, os, pathlib, sys\n"
        "assert sys.argv[1:] == ['audit']\n"
        "root = pathlib.Path(os.environ['NORN_PERFORMANCE_ROOT'])\n"
        "assert root == pathlib.Path.cwd()\n"
        "assert root.joinpath('performance-token').read_text(encoding='utf-8') == os.environ['NORN_PERFORMANCE_TOKEN']\n"
        f"records = json.loads({json.dumps(records)!r})\n"
        f"for record in records: print({PREFIX!r} + json.dumps(record), flush=True)\n"
        f"raise SystemExit({exit_code})\n", encoding="utf-8")
    worker.chmod(0o755)
    return worker, manifest, copy.deepcopy(summary)


class StartupAuditTests(unittest.TestCase):
    def test_worker_documents_are_checked_against_frozen_utf8_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            worker, manifest, expected = fixture(root)
            actual = audit_store(worker, root, manifest)
            self.assertEqual(actual, expected | {"source_sha256": manifest["tree_sha256"]})
            self.assertEqual((root / "data/store.sqlite3").read_bytes(),
                             b"inert test marker; no test opens this file")

    def test_a_missing_store_is_rejected_without_creating_a_database(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, "exactly one store"):
                audit_store(root / "worker", root, {})
            self.assertEqual(list(root.rglob("*")), [])

    def test_stored_document_hashes_lengths_and_paths_must_match_source(self):
        for key, value in (("content_hash", "wrong"), ("byte_length", 999), ("path", "../outside.md")):
            with self.subTest(key=key), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                worker, manifest, _ = fixture(root, lambda rows: rows[0]["document"].update({key: value}))
                with self.assertRaisesRegex(ValueError, "stored document"):
                    audit_store(worker, root, manifest)

    def test_missing_duplicate_and_unordered_documents_are_rejected(self):
        mutations = (
            lambda rows: rows.pop(0),
            lambda rows: rows.insert(0, copy.deepcopy(rows[0])),
            lambda rows: rows.reverse(),
            lambda rows: rows[-1]["summary"]["counts"].update(documents=99),
        )
        for mutate in mutations:
            with self.subTest(mutate=mutate), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                worker, manifest, _ = fixture(root, mutate)
                with self.assertRaises(ValueError):
                    audit_store(worker, root, manifest)

    def test_missing_or_malformed_construction_receipts_are_rejected(self):
        for digest in [None, "short", True, "g" * 64]:
            with self.subTest(digest=digest), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                worker, manifest, _ = fixture(root, lambda rows: rows[-1]["summary"].update(
                    document_construction_sha256=digest))
                with self.assertRaisesRegex(ValueError, "construction"):
                    audit_store(worker, root, manifest)

    def test_changed_frozen_source_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            worker, manifest, _ = fixture(root)
            (root / "vault/a.md").write_text("changed\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "frozen vault"):
                audit_store(worker, root, manifest)

    def test_partial_or_failed_worker_evidence_cannot_succeed(self):
        for mutate, code in ((lambda rows: rows.pop(), 0), (None, 1)):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                worker, manifest, _ = fixture(root, mutate, code)
                with self.assertRaises((ValueError, RuntimeError)):
                    audit_store(worker, root, manifest)
                self.assertTrue((root / "audit.stdout.log").read_bytes().startswith(PREFIX.encode("utf-8")))

    def test_a_stalled_worker_times_out_and_retains_partial_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            worker, manifest, _ = fixture(root)
            worker.write_text(
                f"#!{sys.executable}\nimport sys, time\n"
                "sys.stdout.write('partial record'); sys.stdout.flush()\n"
                "time.sleep(30)\n", encoding="utf-8")
            with patch("startup_audit.TIMEOUT", 0.2), self.assertRaises(TimeoutError):
                audit_store(worker, root, manifest)
            self.assertEqual((root / "audit.stdout.log").read_bytes(), b"partial record")

    def test_audit_keeps_prior_evidence_and_clears_inherited_norn_controls(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            worker, manifest, _ = fixture(root)
            worker.write_text(worker.read_text(encoding="utf-8").replace(
                "assert sys.argv[1:]", "assert 'NORN_FOREIGN_SETTING' not in os.environ\nassert sys.argv[1:]"),
                encoding="utf-8")
            with patch.dict("os.environ", {"NORN_FOREIGN_SETTING": "must-not-reach-the-worker"}):
                audit_store(worker, root, manifest)
                first = (root / "audit.stdout.log").read_bytes()
                audit_store(worker, root, manifest)
            self.assertEqual((root / "audit.stdout.log").read_bytes(), first + first)


if __name__ == "__main__":
    unittest.main()
