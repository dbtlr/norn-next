#!/usr/bin/env python3
"""Keep a tab-separated timing table of recorded runs and report what changed.

A work counter that changes between two recorded commits is a real change in work.
A clock is reported only after a sustained shift: the latest runs each exceed the
median of the runs before them by a ratio and an absolute floor.
"""

import argparse
import csv
import json
import os
import time
from dataclasses import dataclass
from pathlib import Path
from statistics import median

COLUMNS = ("run_at", "commit", "build_sha256", "compiler", "documents", "metric", "value", "unit",
           "status", "load_start", "note")
COLD = ("first_get", "first_find", "ready", "first_query")


@dataclass(frozen=True)
class Thresholds:
    ratio: float = 1.2
    cold_floor_ms: float = 100.0
    warm_floor_ms: float = 0.5
    sustain: int = 3
    window: int = 7
    minimum_reference: int = 3


def write_rows(table: Path, rows: list[dict]) -> None:
    new = not table.exists()
    with table.open("a", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=COLUMNS, delimiter="\t", lineterminator="\n")
        if new:
            writer.writeheader()
        writer.writerows(rows)


def append(table: Path, source: Path) -> None:
    """Append one completed record as one row per metric."""
    record = json.loads(source.read_text())
    if record.get("record_version") != 1 or record.get("completed") is not True or not record.get("rows"):
        raise ValueError("only a completed version 1 record can enter the table")
    identity = {key: record[key] for key in ("run_at", "commit", "build_sha256", "compiler", "load_start")}
    write_rows(table, [{**identity, **{key: "" if row[key] is None else row[key] for key in
                                       ("documents", "metric", "value", "unit", "status", "note")}}
                       for row in record["rows"]])


def skip(table: Path, *, run_at: str, commit: str, load: float, reason: str) -> None:
    """Record a run withheld from measurement, so the gap is visible in the table."""
    write_rows(table, [{"run_at": run_at, "commit": commit, "build_sha256": "", "compiler": "",
                        "documents": "", "metric": "run", "value": "", "unit": "", "status": "skipped",
                        "load_start": load, "note": reason}])


def read(table: Path) -> list[dict]:
    with table.open(newline="") as file:
        return list(csv.DictReader(file, delimiter="\t"))


def runs(rows: list[dict]) -> list[list[dict]]:
    """Group rows by run in table order."""
    grouped: dict[str, list[dict]] = {}
    for row in rows:
        grouped.setdefault(row["run_at"], []).append(row)
    return list(grouped.values())


def skipped(run: list[dict]) -> bool:
    return any(row["status"] == "skipped" for row in run)


def work_changes(measured: list[list[dict]]) -> list[str]:
    """Compare the latest run's counters with the measured run before it."""
    if len(measured) < 2:
        return []
    before, latest = measured[-2], measured[-1]
    old = {(row["documents"], row["metric"]): row["value"] for row in before
           if row["metric"].startswith("work.") and row["status"] == "measured"}
    lines = []
    for documents in sorted({row["documents"] for row in latest}, key=int):
        changed = [f"{row['metric']} {old[documents, row['metric']]} -> {row['value']}" for row in latest
                   if row["documents"] == documents and row["metric"].startswith("work.")
                   and row["status"] == "measured" and old.get((documents, row["metric"]), row["value"]) != row["value"]]
        if not changed:
            continue
        commits = before[0]["commit"], latest[0]["commit"]
        heading = (f"nondeterministic work at {commits[1]}" if commits[0] == commits[1]
                   else f"work change {commits[0]}..{commits[1]}")
        lines.append(f"{heading} at {documents} documents: " + "; ".join(changed))
    return lines


def clock_shifts(measured: list[list[dict]], thresholds: Thresholds) -> list[str]:
    series: dict[tuple[str, str], list[float]] = {}
    for run in measured:
        for row in run:
            if row["unit"] == "ms" and row["status"] == "measured":
                series.setdefault((row["documents"], row["metric"]), []).append(float(row["value"]))
    lines = []
    for (documents, metric), values in sorted(series.items(), key=lambda item: (int(item[0][0]), item[0][1])):
        recent = values[-thresholds.sustain:]
        reference = values[:-thresholds.sustain][-thresholds.window:]
        if len(recent) < thresholds.sustain or len(reference) < thresholds.minimum_reference:
            continue
        baseline = median(reference)
        floor = thresholds.cold_floor_ms if metric in COLD else thresholds.warm_floor_ms
        if all(value >= baseline * thresholds.ratio and value - baseline >= floor for value in recent):
            lines.append(f"clock shift at {documents} documents: {metric} {baseline:.3f} ms -> "
                         + ", ".join(f"{value:.3f}" for value in recent)
                         + f" (over x{thresholds.ratio} and +{floor} ms for {thresholds.sustain} runs)")
    return lines


def compare(rows: list[dict], thresholds: Thresholds) -> list[str]:
    """Report the latest skip, unmeasured metrics, work changes and sustained clock shifts."""
    every = runs(rows)
    measured = [run for run in every if not skipped(run)]
    lines = []
    if every and skipped(every[-1]):
        lines.append(f"latest run {every[-1][0]['run_at']} skipped: {every[-1][0]['note']}")
    if measured:
        lines += [f"{row['status']} at {row['documents']} documents: {row['metric']} {row['note']}".rstrip()
                  for row in measured[-1] if row["status"] not in ("measured", "skipped")]
    return lines + work_changes(measured) + clock_shifts(measured, thresholds)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    add = commands.add_parser("append", help="append a completed record")
    add.add_argument("table", type=Path)
    add.add_argument("record", type=Path)
    withheld = commands.add_parser("skip", help="record a run withheld from measurement")
    withheld.add_argument("table", type=Path)
    withheld.add_argument("--commit", required=True)
    withheld.add_argument("--reason", required=True)
    report = commands.add_parser("compare", help="report work changes and sustained clock shifts")
    report.add_argument("table", type=Path)
    defaults = Thresholds()
    report.add_argument("--ratio", type=float, default=defaults.ratio)
    report.add_argument("--cold-floor-ms", type=float, default=defaults.cold_floor_ms)
    report.add_argument("--warm-floor-ms", type=float, default=defaults.warm_floor_ms)
    report.add_argument("--sustain", type=int, default=defaults.sustain)
    report.add_argument("--window", type=int, default=defaults.window)
    args = parser.parse_args()
    if args.command == "append":
        append(args.table, args.record)
    elif args.command == "skip":
        skip(args.table, run_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), commit=args.commit,
             load=os.getloadavg()[0], reason=args.reason)
    else:
        thresholds = Thresholds(args.ratio, args.cold_floor_ms, args.warm_floor_ms, args.sustain, args.window)
        lines = compare(read(args.table), thresholds) if args.table.exists() else []
        print("\n".join(lines) if lines else "no work change and no sustained clock shift")


if __name__ == "__main__":
    main()
