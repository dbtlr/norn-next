"""A useful first page must not masquerade as a complete loading capability."""
import copy
import unittest
import subprocess
import sys
import os
from unittest.mock import patch
from transport import environment
from pathlib import Path
from loading import validate_stage


class LoadingChecks(unittest.TestCase):
    def test_cold_run_can_disable_worker_profiling(self):
        from loading import run_one
        with patch.object(Path, "mkdir"), patch("loading.Current", side_effect=RuntimeError("worker started")) as worker:
            with self.assertRaisesRegex(RuntimeError, "worker started"):
                run_one(Path("worker"), Path("run"), {}, "baseline", profiling=False)
        self.assertFalse(worker.call_args.kwargs["profiling"])

    def test_whole_get_comparison_preserves_content_and_checks_finding_handles(self):
        from loading import whole_get_answer
        document = {"path": "note.md", "fields": {}, "body": {"text": "Body", "byte_length": 4}}
        for name in ("links", "headings", "blocks", "tags"):
            document[name] = {"items": [], "total": 0}
        document["findings"] = {"items": [{"id": 7, "generation": 3,
            "kind": "broken_link", "target": "missing"}], "total": 1}
        stage = {"projection_epoch": "early", "projection_generation": 3,
                 "answer": {"report": {"shape": "record", "document": document, "rule_sets": []},
                            "unsatisfied": [], "advisories": []}}
        later = copy.deepcopy(stage)
        later["projection_epoch"], later["projection_generation"] = "final", 100
        later["answer"]["report"]["document"]["findings"]["items"][0].update(id=81, generation=90)
        self.assertEqual(whole_get_answer(stage), whole_get_answer(later))
        later["answer"]["report"]["document"]["findings"]["items"][0]["target"] = "different"
        self.assertNotEqual(whole_get_answer(stage), whole_get_answer(later))
        stage["answer"]["report"]["document"]["findings"]["items"][0]["generation"] = 4
        with self.assertRaisesRegex(ValueError, "provenance"):
            whole_get_answer(stage)

    def test_whole_get_comparison_rejects_an_omitted_collection(self):
        from loading import whole_get_answer
        stage = {"projection_epoch": "early", "projection_generation": 3,
                 "answer": {"report": {"shape": "record", "document": {"path": "note.md"}},
                            "unsatisfied": [], "advisories": []}}
        with self.assertRaisesRegex(ValueError, "whole-record"):
            whole_get_answer(stage)

    def test_cache_options_are_explicit_even_when_ambient_options_are_present(self):
        ambient = {"NORN_PERFORMANCE_INITIAL_CACHE_KIB": "123", "NORN_PERFORMANCE_CACHE_DIAGNOSTICS": "1"}
        with patch.dict(os.environ, ambient), patch.object(Path, "mkdir"):
            ordinary = environment(Path("/isolated/run"), "token")
            self.assertNotIn("NORN_PERFORMANCE_INITIAL_CACHE_KIB", ordinary)
            self.assertNotIn("NORN_PERFORMANCE_CACHE_DIAGNOSTICS", ordinary)
            explicit = environment(Path("/isolated/run"), "token", initial_cache_kib=8192,
                                   cache_diagnostics=True)
        self.assertEqual(explicit["NORN_PERFORMANCE_INITIAL_CACHE_KIB"], "8192")
        self.assertEqual(explicit["NORN_PERFORMANCE_CACHE_DIAGNOSTICS"], "1")

    def test_coverage_count_rejects_a_correct_looking_partial_population(self):
        manifest = {"document_count": 3, "active_paths": ["a.md"]}
        stage = {"startup_stage": "frontmatter", "probe": "document_count", "global_ready": False,
                 "command": {"op": "count"}, "answer": {"unsatisfied": [], "advisories": [],
                 "report": {"rows": [{"group": [], "count": 3}], "next": None}}}
        validate_stage(stage, manifest)
        stage["answer"]["report"]["rows"][0]["count"] = 1
        with self.assertRaisesRegex(ValueError, "whole corpus"):
            validate_stage(stage, manifest)

    def test_canonical_demand_requires_whole_record_collections(self):
        manifest = {"small_body": "Body", "small_fields": {}}
        # Whole-record validation must reject a projected body/fields result
        # before generic fixture validation can call it a successful get.
        stage = {"startup_stage": "canonical-demand", "probe": "whole_slug", "global_ready": False,
                 "simulation": "paused", "command": {"op": "get", "target": "small", "whole": True},
                 "answer": {"unsatisfied": [], "advisories": [], "report": {"shape": "record", "document": {"path": "bench/small.md"}}}}
        with self.assertRaisesRegex(ValueError, "whole-record"):
            validate_stage(stage, manifest)

    def test_intake_sparse_and_zero_results_require_complete_exact_rows(self):
        manifest = {"filtered_paths": ["a.md", "b.md"], "active_paths": [], "document_count": 2}
        stage = {"startup_stage": "canonical-intake", "probe": "sparse_page", "global_ready": False,
                 "simulation": "paused", "projection_epoch": "early", "projection_generation": 2,
                 "command": {"op": "find", "value": "probe"}, "answer": {"unsatisfied": [], "advisories": [],
                 "report": {"page": {"rows": [{"path": "a.md"}, {"path": "b.md"}], "next": None, "moved": []}, "rule_sets": []}}}
        validate_stage(stage, manifest)
        stage["answer"]["report"]["page"]["rows"].pop()
        with self.assertRaisesRegex(ValueError, "metadata"):
            validate_stage(stage, manifest)
        stage["probe"] = "zero_page"
        stage["command"]["value"] = "profilenonexistentvalue"
        with self.assertRaisesRegex(ValueError, "metadata"):
            validate_stage(stage, manifest)
        stage["answer"]["report"]["page"]["rows"] = []
        validate_stage(stage, manifest)

    def test_metadata_field_sort_keeps_descending_path_ties(self):
        manifest = {"filtered_paths": ["a.md", "b.md"]}
        stage = {"startup_stage": "canonical-intake", "probe": "field_page", "global_ready": False,
                 "simulation": "paused", "projection_epoch": "early", "projection_generation": 2,
                 "command": {"op": "find", "value": "probe", "field_sort": True},
                 "answer": {"unsatisfied": [], "advisories": [], "report": {"page": {
                     "rows": [{"path": path, "fields": {"perf_bucket": {"raw": "probe"}}} for path in ["b.md", "a.md"]],
                     "next": None, "moved": []}}}}
        validate_stage(stage, manifest)
        stage["answer"]["report"]["page"]["rows"].reverse()
        with self.assertRaisesRegex(ValueError, "ordered match"):
            validate_stage(stage, manifest)

    def test_metadata_normalization_checks_provenance_before_removing_identity(self):
        from loading import metadata_answer
        stage = {"projection_epoch": "early", "projection_generation": 2,
                 "answer": {"report": {"counted_at": {"epoch": "early", "generation": 2}, "next": None}}}
        self.assertNotIn("epoch", metadata_answer(stage)["report"]["counted_at"])
        stage["answer"]["report"]["counted_at"]["epoch"] = "unrelated"
        with self.assertRaisesRegex(ValueError, "provenance"):
            metadata_answer(stage)

    def test_metadata_cursor_normalization_preserves_key_and_schema(self):
        import base64
        import json
        from loading import metadata_answer
        cursor = {"snapshot": {"epoch": "early", "generation": 2, "schema_fingerprint": "schema", "sidecar_revision": None},
                  "key": {"kind": "document", "path": "b.md", "sort": "probe"}}
        encode = lambda: base64.urlsafe_b64encode(json.dumps(cursor).encode()).decode().rstrip("=")
        stage = {"projection_epoch": "early", "projection_generation": 2,
                 "answer": {"report": {"page": {"next": encode()}}}}
        normalized = metadata_answer(stage)["report"]["page"]["next"]
        self.assertEqual(normalized["key"], cursor["key"])
        self.assertEqual(normalized["snapshot"]["schema_fingerprint"], "schema")
        cursor["snapshot"]["generation"] = 99
        stage["answer"]["report"]["page"]["next"] = encode()
        with self.assertRaisesRegex(ValueError, "provenance"):
            metadata_answer(stage)

    def test_warm_runner_rejects_duplicate_modes_before_opening_builds(self):
        command = [sys.executable, str(Path(__file__).with_name("loading_warm.py"))]
        for argument in ("candidate-source", "candidate-build", "baseline-source", "baseline-build", "audit-build", "fixtures", "output"):
            command.extend(["--" + argument, "/missing/experiment/" + argument])
        command.extend(["--modes", "layered-silent", "layered-silent"])
        result = subprocess.run(command, text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn("unique modes", result.stderr)

    def test_catalogue_cannot_claim_unique_when_its_class_is_ambiguous(self):
        stage = {"startup_stage": "catalogue", "probe": "names", "global_ready": False,
                 "candidates": [{"target": "small", "paths": ["bench/small.md"], "total": 1},
                 {"target": "roadmap", "paths": [str(i) for i in range(5)], "total": 6},
                 {"target": "profilenonexistentdocument", "paths": [], "total": 0}]}
        validate_stage(stage, {})
        changed = copy.deepcopy(stage)
        changed["candidates"][1]["total"] = 1
        with self.assertRaisesRegex(ValueError, "ambiguity"):
            validate_stage(changed, {})


if __name__ == "__main__":
    unittest.main()
