#!/usr/bin/env python3
"""Summarize a complete startup experiment without combining nested timing scopes."""

import argparse
import html
import itertools
import json
import statistics
from pathlib import Path

from results import distribution
from startup import partition


KEYS = ("repetition", "background_documents", "changeset_size", "build_kind", "profiling", "stage")
PHASES = {"document_plan": "Document parsing, mapping and judgment",
          "row_and_index_writes": "Document, fact and index writes",
          "link_health": "Link-health maintenance", "commit": "Commit",
          "other_increment": "Other increment work", "other_startup": "Remaining startup"}
WORK = ("document_opens", "documents_derived", "changesets_applied", "links_redecided", "changeset_read_steps")


def validate_samples(metadata, samples):
    if not metadata["completed"]:
        raise ValueError("the startup experiment did not complete")
    expected = {(repetition, size, batch, variant["build_kind"], variant["profiling"], stage)
                for repetition, size, batch, variant, stage in itertools.product(
                    range(metadata["repetitions"]), metadata["sizes"], metadata["batches"],
                    metadata["variants"], ("fresh", "restart"))}
    actual = [tuple(sample[key] for key in KEYS) for sample in samples]
    if len(actual) != len(expected) or set(actual) != expected or not all(sample["verified"] for sample in samples):
        raise ValueError("startup sample identities are missing, duplicated or unverified")


def summarize(metadata, samples):
    validate_samples(metadata, samples)
    summaries = []
    for size, batch, variant, stage in itertools.product(
            metadata["sizes"], metadata["batches"], metadata["variants"], ("fresh", "restart")):
        build_kind, profiling = variant["build_kind"], variant["profiling"]
        chosen = [sample for sample in samples if
                  (sample["background_documents"], sample["changeset_size"], sample["build_kind"], sample["profiling"], sample["stage"])
                  == (size, batch, build_kind, profiling, stage)]
        phases = [partition(sample["ready"]) for sample in chosen]
        row = {"background_documents": size, "document_count": size + 14, "changeset_size": batch,
               "build_kind": build_kind, "profiling": profiling, "stage": stage,
               "first_validated_query": distribution([sample["first_validated_query_ns"] for sample in chosen]),
               "startup_complete": distribution([sample["ready"]["startup_complete_ns"] for sample in chosen]),
               "observed_ready": distribution([sample["ready"]["observed_ready_ns"] for sample in chosen]),
               "first_query": distribution([sample["first_query_host_ns"] for sample in chosen]),
               "repetition_seconds": [sample["first_validated_query_ns"] / 1e9 for sample in chosen],
               "cpu_seconds": statistics.median(sample["resources_at_first_query"]["cpu_seconds"] for sample in chosen),
               "peak_mib": statistics.median(sample["resources_at_first_query"]["VmHWM_bytes"] / 1048576 for sample in chosen),
               "peak_mib_range": [min(sample["resources_at_first_query"]["VmHWM_bytes"] / 1048576 for sample in chosen),
                                  max(sample["resources_at_first_query"]["VmHWM_bytes"] / 1048576 for sample in chosen)],
               "work": {key: [min(sample["ready"]["job_evidence"][key] for sample in chosen),
                              max(sample["ready"]["job_evidence"][key] for sample in chosen)] for key in WORK}
                        if build_kind == "profile" else None}
        if profiling:
            row["phase_mean_ns"] = {key: statistics.mean(phase[key] for phase in phases) for key in PHASES}
        summaries.append(row)
    return summaries


def render(metadata, rows):
    esc = html.escape
    latencies, phases, work = [], [], []
    for row in rows:
        timing = "on" if row["profiling"] else "off"
        label = f'{row["document_count"]:,} / {row["changeset_size"]} / {row["build_kind"]} {timing} / {row["stage"]}'
        latencies.append(f'<tr><th>{label}</th><td>{row["first_validated_query"]["median_ms"] / 1000:.3f}</td>'
                         f'<td>{esc(", ".join(f"{value:.3f}" for value in row["repetition_seconds"]))}</td>'
                         f'<td>{row["cpu_seconds"]:.2f}</td><td>{row["peak_mib"]:.1f}</td></tr>')
        if row["work"] is not None:
            work.append(f'<tr><th>{label}</th>' + ''.join(f'<td>{esc(str(row["work"][key]))}</td>' for key in WORK) + '</tr>')
        if row["profiling"]:
            total = sum(row["phase_mean_ns"].values())
            phases.append(f'<tr><th>{label}</th>' + ''.join(
                f'<td>{row["phase_mean_ns"][key] / 1e9:.3f}<small>{100 * row["phase_mean_ns"][key] / total:.1f}%</small></td>'
                for key in PHASES) + '</tr>')
    return f'''<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Norn startup attribution</title>
<style>body{{margin:0;color:var(--foreground,#222);font:14px/1.5 var(--font-sans,system-ui)}}
main{{max-width:1200px;margin:auto}}h1{{font-size:22px}}h2{{font-size:18px;margin-top:24px}}
table{{width:100%;border-collapse:collapse;font-size:12px;font-variant-numeric:tabular-nums}}
th,td{{padding:8px;border-bottom:1px solid var(--border,#ddd);text-align:left;vertical-align:top}}
tbody th{{font-weight:500}}.wrap{{overflow:auto}}small{{display:block;color:var(--muted-foreground,#666)}}
code{{overflow-wrap:anywhere}}li{{margin-bottom:6px}}</style></head><body><main>
<h1>Norn startup attribution</h1><p>{esc(metadata["started_at"])} · {metadata["repetitions"]} repetitions per configuration.
Store read pages stay at 128 rows; only the changeset bound varies.</p>
<p>All known-answer search/get checks passed. Every document hash matches the frozen source.
Normalized persisted facts and findings agree across configurations. Each unchanged restart preserves its store epoch and write generation.</p>
<h2>Fresh indexing and existing-index restart</h2><p>End-to-end time runs from launching the worker through its first validated common-text query.
CPU and peak RSS are Linux main-process readings at that boundary. Repetition values expose variation; none are removed.</p>
<div class="wrap"><table><thead><tr><th>Documents / batch / timers / stage</th><th>Median s</th><th>Each repetition · s</th><th>Median CPU s</th><th>Median peak MiB</th></tr></thead><tbody>{''.join(latencies)}</tbody></table></div>
<h2>Startup phase attribution</h2><p>Mean seconds across repetitions, partitioned without adding parent and child spans.
The total is worker setup through completed startup bookkeeping; the first query follows this timing window.</p>
<div class="wrap"><table><thead><tr><th>Documents / batch / timers / stage</th>{''.join('<th>' + esc(label) + '</th>' for label in PHASES.values())}</tr></thead><tbody>{''.join(phases)}</tbody></table></div>
<h2>Logical work</h2><p>Minimum and maximum across repetitions. Existing-index restarts still read and hash source documents.</p>
<div class="wrap"><table><thead><tr><th>Documents / batch / timers / stage</th>{''.join('<th>' + key.replace('_', ' ') + '</th>' for key in WORK)}</tr></thead><tbody>{''.join(work)}</tbody></table></div>
<h2>Scope and interpretation</h2><ul>
<li>Real production host paths run through a development worker. Profile builds enable the host's harness-only induced-failure feature; standard builds do not. Fault environment variables are removed.</li>
<li>Timers are explicitly armed per process and retain only bounded aggregates. Timing-on and timing-off controls use the same executable; compare them before treating instrumented clocks as absolute budgets.</li>
<li>Row and index writes include FTS triggers, fact rows and filename trigrams. The raw document-row subspan includes parameter preparation and triggers; it does not isolate FTS alone.</li>
<li>Nested attach, heal, parsing and database spans remain in raw evidence. Production attach ends before lifecycle bookkeeping and possible follow-up reconciliation. The partition uses disjoint planning and increment work across that complete window.</li>
<li>Watcher background work can overlap caller intervals. These are elapsed scopes, not per-phase CPU samples.</li>
<li>All cells run serially. Size and batch order rotate, and variant order reverses on alternating repetitions. The audit reads source files and the store before restart, deliberately warming caches. This is a cache-warmed existing-index restart. Other host activity is uncontrolled.</li>
<li>The persisted-state check normalizes surrogate IDs, generations and timestamps. FTS vocabulary and document token sizes are compared, alongside authored search answers; every term-to-document posting is not independently audited.</li>
<li>This experiment measures startup and immediate restart, not long-term retained memory. No production defaults are changed.</li></ul>
<h2>Identity and evidence</h2><p>Executable SHA-256: <code>{esc(metadata["current_build"]["sha256"])}</code><br>
Compiled source SHA-256: <code>{esc(metadata["current_build"]["source_tree_sha256"])}</code><br>
Source base: <code>{esc(metadata["current_build"]["repository_head"])}</code><br>
Platform: {esc(metadata["platform"])}<br>System load start/end: {esc(str(metadata["load_start"]))} / {esc(str(metadata["load_end"]))}</p>
<p>run.json preserves build and fixture identity. samples.jsonl preserves every clock, inclusive scope, counter and semantic audit.
The frozen worker and per-stage logs remain with the run. summary.json preserves the distributions shown here.</p>
</main></body></html>'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    args = parser.parse_args()
    metadata = json.loads((args.run / "run.json").read_text())
    samples = [json.loads(line) for line in (args.run / "samples.jsonl").read_text().splitlines()]
    rows = summarize(metadata, samples)
    (args.run / "summary.json").write_text(json.dumps(rows, indent=2) + "\n")
    (args.run / "report.html").write_text(render(metadata, rows), encoding="utf-8")
    print(args.run / "report.html")


if __name__ == "__main__":
    main()
