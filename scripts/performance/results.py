"""Independent answer checks and summaries for the baseline workload."""

import math
import statistics

from fixture import COMMON, RARE, MISSING


def validate_paths(actual: list[str], expected: list[str], limit: int) -> None:
    if len(actual) != min(len(expected), limit):
        raise ValueError(f"wrong answer size: {len(actual)} for {len(expected)} matches")
    if len(set(actual)) != len(actual):
        raise ValueError("duplicate result paths")
    if not set(actual) <= set(expected):
        raise ValueError("a result does not match the authored workload")


def payload(engine: str, response: dict) -> dict:
    if engine == "current":
        answer = response["answer"]
        if answer.get("unsatisfied"):
            raise ValueError(f"current request was partially satisfied: {answer['unsatisfied']}")
        return answer["report"]
    if response.get("isError"):
        raise ValueError("legacy tool reported failure")
    answer = response["structuredContent"]
    if answer.get("section_failures") or answer.get("has_diagnostic_errors"):
        raise ValueError("legacy answer contains diagnostic failures")
    return answer


def record(engine: str, response: dict) -> dict:
    answer = payload(engine, response)
    if engine == "current":
        document = answer["document"]
        body = document["body"]
        if len(body["text"].encode()) != body["byte_length"]:
            raise ValueError("current get returned a truncated body")
        return {"path": document["path"], "body": body["text"],
                "fields": {key: value["raw"] for key, value in document["fields"].items()}}
    if len(answer["records"]) != 1:
        raise ValueError("get must return exactly one document")
    document = answer["records"][0]
    return {"path": document["path"], "body": document["body"],
            "fields": document["frontmatter"]}


def validate_read(engine: str, command: dict, response: dict, manifest: dict) -> None:
    answer = payload(engine, response)
    if engine == "current" and response["answer"].get("advisories"):
        raise ValueError("the fixed lexical/read workload returned an unexpected advisory")
    if command["op"] in ("search", "find"):
        if command["op"] == "find":
            expected = manifest["active_paths"]
        elif command["query"] == RARE:
            expected = manifest["rare_paths"]
        elif command["query"] == MISSING:
            expected = []
        elif command["query"] == COMMON:
            expected = manifest["filtered_paths"] if "path" in command else manifest["paths"]
        else:
            raise ValueError("a query has no independent expected answer")
        rows = answer["page"]["rows"] if engine == "current" else answer["documents"]
        paths = [row["path"] for row in rows]
        validate_paths(paths, expected, 25)
        if engine == "legacy" and answer["total"] != len(expected):
            raise ValueError("legacy exact total is wrong")
        if command["op"] == "find":
            fold = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")
            ordered = sorted(expected, key=lambda path: (path.translate(fold), path)) if engine == "current" else sorted(expected)
            if paths != ordered[:25]:
                raise ValueError("metadata query returned the wrong first page")
    elif "#" in command["target"]:
        if engine == "current":
            actual = answer["body"]["text"]
            expected = manifest["section"].split("\n", 1)[1]
            if answer["path"] != "bench/large.md" or answer["heading"]["text"] != "Details":
                raise ValueError("current section target is wrong")
        else:
            actual = answer["records"][0]["sections"]["Details"]
            expected = manifest["section"]
        if actual != expected:
            raise ValueError("section content differs from the fixture")
    else:
        document = record(engine, response)
        expected = manifest["small_body" if command["target"] == "bench/small.md" else "large_body"]
        if document != {"path": command["target"], "body": expected,
                        "fields": {"status": "draft", "perf_bucket": "probe"}}:
            raise ValueError("get differs from the fixture's body or fields")


def validate_write(engine: str, response: dict) -> None:
    answer = payload(engine, response)
    report = answer if engine == "current" else answer["report"]
    if report["outcome"] != "applied":
        raise ValueError("the write was not applied")
    if engine == "current" and report["changeset"] != "committed":
        raise ValueError("the current write did not commit its derived state")
    if engine == "legacy" and (report.get("failed", 0) or report.get("skipped", 0) or report.get("remaining", 0)):
        raise ValueError("the legacy write was incomplete")


def distribution(values: list[int]) -> dict:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("no successful samples")
    return {"samples": len(values), "median_ms": statistics.median(ordered) / 1e6,
            "p95_ms": ordered[math.ceil(0.95 * len(ordered)) - 1] / 1e6,
            "min_ms": ordered[0] / 1e6, "max_ms": ordered[-1] / 1e6}
