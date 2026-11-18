#!/usr/bin/env python3
"""Assemble the stored legacy record from its recorded parts, and judge whether it still applies.

Legacy Norn 0.48.3 rescans the vault on every write, so recording it at the
documented settings takes hours. It is recorded once, one process of one size on
each of several runners, and the parts are assembled into one record that is
committed beside this script. The nightly sets that stored record beside the
engines it measures, but only while its key still holds: the same legacy
executable, the same fixture bytes at each size and the same record version. A
key that no longer holds is reported with its reasons, never compared.
"""

import argparse
import json
import os
import tarfile
import time
from pathlib import Path
from statistics import median

from record import RECORD_VERSION, resource_rows, summarize

STORED = Path(__file__).resolve().parent / "legacy-record.json"
# The settings docs/performance.md promises for a timing record. An assembled
# record holds exactly these, so the stored record is never a reduced one.
SIZES = (2000, 10000)
PROCESSES = 3
SETTINGS = {"samples": 400, "warmups": 5, "write_payoff": 200}
# What a run sets besides the sizes (its fixtures judge those) that changes what
# legacy's numbers are. The record carries each at its top level.
MEASURED = ("processes", *SETTINGS)


def key(legacy_sha256, fixtures):
    """What a legacy record must share with a run to be compared with it.

    `fixtures` maps each background size, as a string, to its fixture's tree digest.
    """
    return {"legacy_sha256": legacy_sha256, "fixtures": dict(fixtures), "record_version": RECORD_VERSION}


def mismatches(stored, expected, measured):
    """Why the stored record's key does not hold for `expected`; empty when it does.

    `measured` maps each name in `MEASURED` to the value this run sets; the stored
    record must have been recorded at the same values.

    Only the sizes `expected` measures are judged: a stored size the run does not
    measure is never shown, so it cannot be compared stale.
    """
    held = stored.get("key") or {}
    reasons = []
    if held.get("record_version") != expected["record_version"]:
        reasons.append(f"the record version differs: stored {held.get('record_version')}, "
                       f"this run {expected['record_version']}")
    if held.get("legacy_sha256") != expected["legacy_sha256"]:
        reasons.append(f"the legacy executable differs: stored {short(held.get('legacy_sha256'))}, "
                       f"this run {short(expected['legacy_sha256'])}")
    stored_fixtures = held.get("fixtures") or {}
    for size, digest in sorted(expected["fixtures"].items(), key=lambda item: int(item[0])):
        if size not in stored_fixtures:
            reasons.append(f"the stored record has no {size}-document fixture")
        elif stored_fixtures[size] != digest:
            reasons.append(f"the {size}-document fixture differs: stored {short(stored_fixtures[size])}, "
                           f"this run {short(digest)}")
    for name in MEASURED:
        if stored.get(name) != measured[name]:
            reasons.append(f"the {name.replace('_', ' ')} setting differs: stored {stored.get(name)}, "
                           f"this run {measured[name]}")
    return reasons


def short(digest):
    return f"`{digest[:12]}`" if digest else "none"


def part_records(directory):
    """Every legacy part record under `directory`, read from each evidence archive."""
    parts = []
    for archive in sorted(directory.rglob("evidence.tar.gz")):
        with tarfile.open(archive) as tar:
            member = tar.extractfile("legacy/record.json")
            parts.append((archive.parent.name, json.load(member)))
    return parts


def assemble(parts, *, sizes=SIZES, processes=PROCESSES, settings=SETTINGS, provenance=None):
    """One record from single-process parts, refusing any part that does not belong.

    Every part must be a completed legacy record of this record version, at the
    documented settings, measuring one process of one size with the same
    executable; the parts of a size must share their fixture; and each size must
    have exactly `processes` parts. Rows are summarized over the parts' processes
    exactly as one multi-process record summarizes its own.
    """
    if not parts:
        raise ValueError("no legacy part was found")
    first = parts[0][1]
    fixtures, by_size = {}, {}
    for name, part in parts:
        problem = None
        if part.get("engine") != "legacy" or part.get("completed") is not True:
            problem = "is not a completed legacy record"
        elif part.get("record_version") != RECORD_VERSION:
            problem = f"has record version {part.get('record_version')}, not {RECORD_VERSION}"
        elif any(part.get(setting) != value for setting, value in settings.items()):
            problem = "was not recorded at the documented settings"
        elif part.get("processes") != 1 or len(part.get("sizes", [])) != 1 or len(part.get("process_evidence", [])) != 1:
            problem = "does not hold exactly one process of one size"
        elif (part.get("build_sha256"), part.get("commit")) != (first.get("build_sha256"), first.get("commit")):
            problem = "measured another legacy executable"
        if problem:
            raise ValueError(f"part {name} {problem}")
        size = str(part["sizes"][0])
        digest = part["fixtures"][size]
        if fixtures.setdefault(size, digest) != digest:
            raise ValueError(f"part {name} measured another {size}-document fixture")
        by_size.setdefault(size, []).append((name, part))
    if sorted(by_size, key=int) != [str(size) for size in sizes]:
        raise ValueError(f"the parts measure sizes {sorted(by_size, key=int)}, not {list(map(str, sizes))}")
    for size, group in by_size.items():
        if len(group) != processes:
            raise ValueError(f"{len(group)} parts measure {size} documents, not {processes}")
    ordered = [part for size in sorted(by_size, key=int) for part in by_size[size]]
    evidence = [part["process_evidence"][0] for _, part in ordered]
    rows = summarize(evidence) + resource_rows(evidence)
    return {
        "record_version": RECORD_VERSION, "engine": "legacy", "completed": True,
        "key": key(first["build_sha256"], fixtures),
        "commit": first["commit"], "build_sha256": first["build_sha256"], "compiler": first["compiler"],
        "run_at": min(part["run_at"] for _, part in ordered),
        "finished_at": max(part["finished_at"] for _, part in ordered),
        "load_start": median(part["load_start"] for _, part in ordered),
        "sizes": list(sizes), "processes": processes, **settings,
        "recorded": provenance or {},
        "parts": [{"name": name, "size": part["sizes"][0], "run_at": part["run_at"],
                   "finished_at": part["finished_at"], "load_start": part["load_start"],
                   "load_end": part["load_end"]} for name, part in ordered],
        "rows": rows,
    }


def github_provenance():
    """Where the recording ran, read from the Actions environment when there is one."""
    environment = os.environ
    if not environment.get("GITHUB_RUN_ID"):
        return {"assembled_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    url = f"{environment['GITHUB_SERVER_URL']}/{environment['GITHUB_REPOSITORY']}/actions/runs/{environment['GITHUB_RUN_ID']}"
    return {"run_id": environment["GITHUB_RUN_ID"], "run_attempt": environment.get("GITHUB_RUN_ATTEMPT"),
            "workflow_sha": environment.get("GITHUB_SHA"), "ref": environment.get("GITHUB_REF_NAME"), "url": url,
            "assembled_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("assemble", help="assemble part evidence archives into one stored record")
    build.add_argument("parts", type=Path, help="directory holding each part's evidence.tar.gz")
    build.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    record = assemble(part_records(args.parts), provenance=github_provenance())
    args.output.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    print(args.output)


if __name__ == "__main__":
    main()
