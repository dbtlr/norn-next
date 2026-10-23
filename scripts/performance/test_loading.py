"""A useful first page must not masquerade as a complete loading capability."""
import copy
import unittest
import subprocess
import sys
from pathlib import Path
from loading import validate_stage


class LoadingChecks(unittest.TestCase):
    def test_coverage_count_rejects_a_correct_looking_partial_population(self):
        manifest = {"document_count": 3, "active_paths": ["a.md"]}
        stage = {"startup_stage": "frontmatter", "probe": "document_count", "global_ready": False,
                 "command": {"op": "count"}, "answer": {"unsatisfied": [], "advisories": [],
                 "report": {"rows": [{"group": [], "count": 3}], "next": None}}}
        validate_stage(stage, manifest)
        stage["answer"]["report"]["rows"][0]["count"] = 1
        with self.assertRaisesRegex(ValueError, "whole corpus"):
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
