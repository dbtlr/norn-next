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
