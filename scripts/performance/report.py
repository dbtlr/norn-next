"""Render a completed baseline as portable JSON and a self-contained HTML report."""

import argparse
import html
import json
from pathlib import Path

from baseline import READS, WRITES
from results import distribution


LABELS = {
    "search_rare": "Rare text · 1 match", "search_common": "Common text · first 25",
    "search_missing": "Missing text · 0 matches", "search_filtered": "Text within bench/ · 14 matches",
    "find_metadata": "Metadata · first 25", "get_small": "Get · about 2 KiB",
    "get_large": "Get · about 48 KiB", "get_section": "Get · section in large document",
    "set_field": "Set one field", "create": "Create one document",
    "rename": "Rename · no backlinks", "rename_hub": "Rename · 10 backlinks",
}


def summarize(metadata: dict, observations: list[dict]) -> list[dict]:
    if not metadata["completed"]:
        raise ValueError("the run did not complete; inspect run.json and preserve partial evidence")
    scenarios = [name for name, _ in READS] + WRITES
    rows = []
    for size in metadata["sizes"]:
        for scenario in scenarios:
            row = {"background_documents": size, "scenario": scenario}
            for engine in ("current", "legacy"):
                selected = [item for item in observations if item["kind"] == "sample"
                            and item["background_documents"] == size and item["scenario"] == scenario
                            and item["engine"] == engine and not item["warmup"]]
                if len(selected) != metadata["samples"] * metadata["repetitions"] or not all(item["verified"] for item in selected):
                    raise ValueError(f"incomplete verified samples for {engine}/{size}/{scenario}")
                expected = {(repetition, iteration) for repetition in range(metadata["repetitions"])
                            for iteration in range(metadata.get("warmups", 0), metadata.get("warmups", 0) + metadata["samples"])}
                if {(item["repetition"], item["iteration"]) for item in selected} != expected:
                    raise ValueError("sample identities are missing or duplicated")
                result = distribution([item["round_trip_ns"] for item in selected])
                result["round_medians_ms"] = []
                for repetition in range(metadata["repetitions"]):
                    values = [item["round_trip_ns"] for item in selected if item["repetition"] == repetition]
                    if len(values) != metadata["samples"]:
                        raise ValueError("one process repetition has missing or duplicated samples")
                    result["round_medians_ms"].append(distribution(values)["median_ms"])
                if scenario in WRITES:
                    result["write_and_readback"] = distribution([item["write_and_readback_ns"] for item in selected])
                if engine == "current":
                    result["host"] = distribution([item["host_ns"] for item in selected])
                    result["counter_ranges"] = {
                        key: [min(item["counters"][key] for item in selected),
                              max(item["counters"][key] for item in selected)]
                        for key in selected[0]["counters"]}
                row[engine] = result
            row["current_over_legacy"] = row["current"]["median_ms"] / row["legacy"]["median_ms"]
            rows.append(row)
    return rows


def render(metadata, rows, observations):
    esc = html.escape
    largest = max(metadata["sizes"])
    startup_at_largest = {engine: distribution([item["first_validated_query_ns"] for item in observations
        if item["kind"] == "startup" and item["background_documents"] == largest and item["engine"] == engine])["median_ms"] / 1000
        for engine in ("current", "legacy")}
    warm_wins = sum(row["current_over_legacy"] < 1 for row in rows)
    sections = []
    for size in metadata["sizes"]:
        body = []
        for row in (row for row in rows if row["background_documents"] == size):
            current, legacy = row["current"], row["legacy"]
            ratio = row["current_over_legacy"]
            change = f"{(1 - ratio) * 100:.1f}% lower" if ratio < 1 else f"{(ratio - 1) * 100:.1f}% higher"
            body.append(f'<tr><th>{esc(LABELS[row["scenario"]])}</th>'
                        f'<td>{current["median_ms"]:.3f}<small>p95 {current["p95_ms"]:.3f}</small></td>'
                        f'<td>{legacy["median_ms"]:.3f}<small>p95 {legacy["p95_ms"]:.3f}</small></td>'
                        f'<td>{change}</td></tr>')
        sections.append(f'<h2>{size + 14:,} documents</h2><div class="table-wrap"><table><thead><tr>'
                        '<th>Operation</th><th>Current · ms</th><th>Legacy · ms</th><th>Current vs legacy</th>'
                        '</tr></thead><tbody>' + ''.join(body) + '</tbody></table></div>')

    memory = []
    for item in observations:
        if item["kind"] != "end":
            continue
        values = item["resources"]
        start = next(start for start in observations if start["kind"] == "startup"
                     and all(start[key] == item[key] for key in ("engine", "background_documents", "repetition")))
        before = start["resources"]
        memory.append(f'<tr><td>{item["document_count"]:,}</td><td>{item["engine"]}</td>'
                      f'<td>{item["repetition"] + 1}</td><td>{before["VmRSS_bytes"] / 1048576:.1f}</td>'
                      f'<td>{values["VmRSS_bytes"] / 1048576:.1f}</td><td>{values["VmHWM_bytes"] / 1048576:.1f}</td>'
                      f'<td>{before["open_fds"]} → {values["open_fds"]}</td>'
                      f'<td>{values["cpu_seconds"] - before["cpu_seconds"]:.2f}</td></tr>')
    writes = []
    for row in rows:
        if row["scenario"] in WRITES:
            writes.append(f'<tr><td>{row["background_documents"] + 14:,}</td><th>{LABELS[row["scenario"]]}</th>'
                          f'<td>{row["current"]["write_and_readback"]["median_ms"]:.3f}</td>'
                          f'<td>{row["legacy"]["write_and_readback"]["median_ms"]:.3f}</td></tr>')
    counters = []
    for row in rows:
        ranges = row["current"].get("counter_ranges", {})
        if ranges:
            counters.append(f'<tr><td>{row["background_documents"] + 14:,}</td><th>{LABELS[row["scenario"]]}</th>'
                            f'<td>{esc(str(ranges["statements_executed"]))}</td>'
                            f'<td>{esc(str(ranges["vm_steps"]))}</td><td>{esc(str(ranges["full_scan_steps"]))}</td></tr>')
    startups = []
    for item in observations:
        if item["kind"] == "startup":
            setup = item["ready"].get("host_setup_ns")
            setup_text = f'{setup / 1e6:.3f}' if setup is not None else '—'
            startups.append(f'<tr><td>{item["document_count"]:,}</td><td>{item["engine"]}</td>'
                            f'<td>{item["repetition"] + 1}</td><td>{item["first_validated_query_ns"] / 1e9:.3f}</td>'
                            f'<td>{setup_text}</td></tr>')
    count = metadata["samples"] * metadata["repetitions"]
    return f'''<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Norn runtime baseline</title>
<style>
body{{font:14px/1.5 var(--font-sans,system-ui);color:var(--foreground,#222);margin:0}}
main{{max-width:1100px;margin:auto}}h1{{font-size:22px;margin:0 0 8px}}h2{{font-size:18px;margin:26px 0 10px}}
p{{margin:8px 0 14px}}.note,small{{color:var(--muted-foreground,#666)}}small{{display:block;font-size:11px}}
table{{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums;font-size:13px}}
th,td{{text-align:left;padding:9px 8px;border-bottom:1px solid var(--border,#ddd);vertical-align:top}}
thead th{{background:var(--muted,#eee);font-size:12px}}tbody th{{font-weight:500}}
.table-wrap{{overflow-x:auto}}code{{font-size:12px;overflow-wrap:anywhere}}details{{margin-top:18px}}
summary{{cursor:pointer;font-weight:650}}li{{margin-bottom:8px}}.scope{{border-left:3px solid var(--border,#aaa);padding-left:14px}}
</style></head><body><main>
<h1>Norn runtime baseline</h1>
<p>{esc(metadata["started_at"])} · {count} measured calls per operation and engine ·
{metadata["repetitions"]} fresh process runs · {metadata["warmups"]} warmups per operation and run.</p>
<p class="scope"><strong>Scope:</strong> current production host through a development JSONL adapter versus
legacy {esc(metadata["legacy_version"])} through its warm MCP daemon. These are observed workflow costs with
different frontends and contracts. They do not establish a pure engine speedup or predict the unfinished current CLI.</p>
<p>Every read was checked against authored fixture answers. Every write was applied and read back;
the hub rename also checked all ten rewritten files. All measurements below come from a completed run.</p>
<p><strong>Warm calls and fresh-store startup:</strong> current had lower median request latency in
{warm_wins} of {len(rows)} workload/size comparisons. At {largest + 14:,} documents, startup from an empty
derived store through the first validated query measured <strong>{startup_at_largest["current"]:.2f} s current</strong>
and <strong>{startup_at_largest["legacy"]:.2f} s legacy</strong> (median of the recorded starts).
Fresh-store startup and warm requests are separate costs.</p>
{''.join(sections)}
<h2>Writes plus immediate readback</h2><p class="note">Sum of the write reply and its confirming get, in milliseconds.
Filesystem checks run outside these clocks. This exposes work deferred until the next read.</p>
<div class="table-wrap"><table><thead><tr><th>Documents</th><th>Operation</th><th>Current · ms</th><th>Legacy · ms</th></tr></thead><tbody>{''.join(writes)}</tbody></table></div>
<h2>Process memory</h2><p class="note">Linux main-process RSS, in MiB. Peak is the process's own VmHWM and includes initial attach.
End readings follow all work and a one-second pause. These short runs cannot establish a leak or a long-term retention slope.
The controller, generator and legacy background child processes are excluded. CPU seconds cover all operations and warmups after readiness.</p>
<div class="table-wrap"><table><thead><tr><th>Documents</th><th>Engine</th><th>Run</th><th>Ready RSS</th><th>End RSS</th><th>Peak RSS</th><th>Open FDs</th><th>CPU s</th></tr></thead><tbody>{''.join(memory)}</tbody></table></div>
<h2>Fresh-store startup</h2><p>Startup is process launch through the first validated common-text query.
Stores are fresh; OS caches are uncontrolled. It includes setup and observation overhead.</p>
<p>Current host setup includes case detection, construction and acquisition of the shared test-watcher lease.
A large setup time can reflect another test holding that lease; it is not indexing time.</p>
<table><thead><tr><th>Documents</th><th>Engine</th><th>Run</th><th>Total seconds</th><th>Host setup ms</th></tr></thead><tbody>{''.join(startups)}</tbody></table>
<details><summary>Logical work counters</summary>
<p>Current snapshot counter ranges across measured calls. SQLite VM steps do not count all internal FTS posting-list work.</p>
<div class="table-wrap"><table><thead><tr><th>Documents</th><th>Operation</th><th>Statements</th><th>VM steps</th><th>Full-scan steps</th></tr></thead><tbody>{''.join(counters)}</tbody></table></div></details>
<h2>How to interpret the comparison</h2><ul>
<li>Search uses isolated whole-word tokens. Legacy matches body substrings, sorts paths and computes an exact total.
Current uses ranked lexical search and bounded pages without an exact total. Common-result ordering differs.
Only the lexical rung runs; vector/fusion candidate-depth limits do not apply. Unexpected read advisories fail the run.</li>
<li>Metadata ordering folds ASCII case in current Norn and uses the legacy path order on the other side.
Each first page is verified against its own declared order.</li>
<li>Both full gets return the same complete body and frontmatter for the probes. Their additional facets differ.
Legacy section get includes its heading and document context; current returns the section body and heading metadata.</li>
<li>Legacy MCP sends both text and structured content. Current uses a development envelope with counters.
Round-trip clocks include those transport, serialization and controller costs. Current host-only times are retained separately.</li>
<li>The current worker is a Cargo release integration-test build, using the existing 128-row/chunk host policy,
one lifecycle worker, native watching and a 50 ms watch poll. Test dependency features are present; fault environment variables are removed.</li>
<li>The legacy binary is identified by hash and daemon build ID, with its cache channel explicitly set to dev.
Its original compiler and optimization flags are unknown.
This comparison is to the installed application, not a controlled same-toolchain rebuild.</li>
<li>The background comes from one realistic generator shape with varying document count, no symlinks and fixed seed 7.
Fourteen known-answer documents are added. Creates add {metadata["samples"] + metadata["warmups"]} documents per run, equally on both sides.</li>
<li>Engines run serially, reversing order on alternating runs. Warmups are retained but excluded from summaries.
No outliers are removed. p95 is descriptive at this sample count; these observations are not acceptance thresholds.</li>
</ul>
<details><summary>Build and run identity</summary><p>Current executable source base: <code>{esc(metadata["current_build"]["repository_head"])}</code><br>
Runner source base: <code>{esc(metadata["repository_head"])}</code><br>
Built source tree SHA-256: <code>{esc(metadata["current_build"]["source_tree_sha256"])}</code><br>
Build checkout dirty: {bool(metadata["current_build"]["repository_status"])}<br>
Current binary SHA-256: <code>{esc(metadata["current_sha256"])}</code><br>
Legacy binary SHA-256: <code>{esc(metadata["legacy_sha256"])}</code><br>
Workload: <code>{esc(metadata["workload_version"])}</code><br>
Platform: {esc(metadata["platform"])}<br>CPU: {esc(metadata["cpu"])}<br>
System load at start/end: {esc(str(metadata["load_start"]))} / {esc(str(metadata["load_end"]))}</p>
<p>Workspace library features: <code>{esc(json.dumps(metadata["current_build"]["workspace_library_features"], sort_keys=True))}</code></p>
<p>Raw observations: samples.jsonl. Build provenance and outcome: run.json. Frozen corpus and exact answers:
fixtures/*/manifest.json. Per-operation examples and daemon logs: work/. summary.json preserves per-run medians and counters.</p></details>
</main></body></html>'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    args = parser.parse_args()
    metadata = json.loads((args.run / "run.json").read_text())
    observations = [json.loads(line) for line in (args.run / "samples.jsonl").read_text().splitlines()]
    rows = summarize(metadata, observations)
    (args.run / "summary.json").write_text(json.dumps(rows, indent=2) + "\n")
    (args.run / "report.html").write_text(render(metadata, rows, observations), encoding="utf-8")
    print(args.run / "report.html")


if __name__ == "__main__":
    main()
