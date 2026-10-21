#!/usr/bin/env python3
"""Record the checkout a full regression job actually tests, and refuse a mismatch."""

import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys


def main():
    if len(sys.argv) != 2:
        print("usage: regression-identity.py <receipt-path>", file=sys.stderr)
        return 2

    checkout = subprocess.run(
        ["git", "rev-parse", "--verify", "HEAD"], capture_output=True, text=True
    )
    identities = {
        "candidate_sha": os.environ.get("NORN_REGRESSION_SHA", ""),
        "event_sha": os.environ.get("NORN_REGRESSION_EVENT_SHA", ""),
        "workflow_sha": os.environ.get("NORN_REGRESSION_WORKFLOW_SHA", ""),
        "checkout_sha": checkout.stdout.strip() if checkout.returncode == 0 else "",
    }
    # The input asserts identity. It never selects a different checkout or workflow.
    matches = all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in identities.values())
    matches = bool(matches and len(set(identities.values())) == 1)
    receipt = {
        "schema_version": 1,
        "suite_contract": "active-regression-v1",
        **identities,
        "identity_matches": matches,
        "job": os.environ.get("GITHUB_JOB", ""),
        "run_id": os.environ.get("GITHUB_RUN_ID", ""),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
        "os": platform.system(),
        "arch": platform.machine(),
    }
    rendered = json.dumps(receipt, indent=2, sort_keys=True) + "\n"
    Path(sys.argv[1]).write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    if not matches:
        print(
            "::error::Regression candidate, event, workflow and checkout must name one full SHA.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
