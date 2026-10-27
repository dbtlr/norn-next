"""Independent answer checks and summaries for the baseline workload."""

import base64
import copy
import json
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


CONTENT_COLUMNS = ("fields", "body", "links", "headings", "blocks", "tags")


def known(value):
    """The value inside a known content carrier; unknown content is never a timed success."""
    if (isinstance(value, dict) and value.get("state") in ("known", "unknown")
            and set(value) <= {"state", "value", "reason"}):
        if value["state"] != "known":
            raise ValueError(f"current answer content is unknown: {value.get('reason')}")
        return value["value"]
    return value


def decode_content(report: dict) -> dict:
    """Unwrap the wire's known/unknown content carriers in place, once, before any check reads them."""
    document = report.get("document")
    if isinstance(document, dict):
        for column in CONTENT_COLUMNS:
            if column in document:
                document[column] = known(document[column])
    if "body" in report:
        report["body"] = known(report["body"])
    return report


def payload(engine: str, response: dict) -> dict:
    if engine == "current":
        answer = response["answer"]
        if answer.get("unsatisfied"):
            raise ValueError(f"current request was partially satisfied: {answer['unsatisfied']}")
        return decode_content(answer["report"])
    if response.get("isError"):
        raise ValueError("legacy tool reported failure")
    answer = response["structuredContent"]
    if answer.get("section_failures") or answer.get("has_diagnostic_errors"):
        raise ValueError("legacy answer contains diagnostic failures")
    return answer


def record(engine: str, response: dict) -> dict:
    answer = payload(engine, response)
    if engine == "current":
        if response["answer"].get("advisories"):
            raise ValueError(f"unexpected confirming-get advisory: {response['answer']['advisories']}")
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


def whole_get_answer(stage, *, first_index=False):
    """Keep the complete record while comparing finding content across store epochs."""
    decode_content(stage["answer"]["report"])
    answer = copy.deepcopy(stage["answer"])
    report = answer["report"]
    document = report.get("document", {})
    required = {"path", "body", "fields", "headings", "blocks", "tags", "links", "findings"}
    if report.get("shape") != "record" or not required <= document.keys():
        raise ValueError("whole-record comparison cannot omit requested collections")
    if answer.get("unsatisfied") != [] or answer.get("advisories") != []:
        raise ValueError("whole-record answer was not fully satisfied")
    reading = answer.get("reading")
    if reading is not None:
        epoch, generation = reading.get("epoch"), reading.get("generation")
        if (reading.get("trust") != {"state": "ready"}
                and not (first_index and reading.get("trust", {}).get("state") == "warming"
                         and reading["trust"].get("phase") == "healing")):
            raise ValueError("whole-record answer provenance is not Ready")
        if "projection_epoch" in stage and (epoch != stage["projection_epoch"]
                or generation != stage["projection_generation"]):
            raise ValueError("whole-record projection provenance disagrees with its answer")
        answer.pop("reading")
    else:
        epoch, generation = stage.get("projection_epoch"), stage.get("projection_generation")
    if not isinstance(epoch, str) or not epoch or type(generation) is not int or generation < 0:
        raise ValueError("whole-record projection provenance is missing or invalid")
    for key in ("headings", "blocks", "tags", "links", "findings"):
        collection = document[key]
        if (not isinstance(collection, dict) or type(collection.get("total")) is not int
                or not isinstance(collection.get("items"), list)
                or collection["total"] != len(collection["items"])):
            raise ValueError("whole-record collection is truncated or has an incorrect total")
    findings = document["findings"]
    finding_ids = set()
    for finding in findings["items"]:
        if (type(finding.get("id")) is not int or finding["id"] <= 0
                or type(finding.get("generation")) is not int
                or not 0 <= finding["generation"] <= generation):
            raise ValueError("whole-record finding provenance is outside its projection")
        if finding["id"] in finding_ids:
            raise ValueError("whole-record finding handles contain a duplicate")
        finding_ids.add(finding["id"])
        finding.pop("id")
        finding.pop("generation")
    return answer


def validate_first_get(response, manifest, *, private=False):
    """Validate the native slug answer against authored fixture content before timing it."""
    if private:
        if (response.get("simulation") != "paused" or response.get("global_ready") is not False
                or "reading" in response.get("answer", {})):
            raise ValueError("private whole-record provenance is invalid")
    elif not isinstance(response.get("answer", {}).get("reading"), dict):
        raise ValueError("native whole-record provenance is missing")
    return validate_first_get_content(response, manifest)


def validate_first_get_content(response, manifest, *, first_index=False):
    """Shared authored whole-record oracle after the caller validates its envelope."""
    normalized = whole_get_answer(response, first_index=first_index)
    validate_read("current", {"op": "get", "target": "bench/small.md"}, response, manifest)
    source = "---\nstatus: draft\nperf_bucket: probe\n---\n" + manifest["small_body"]

    def span(token):
        offset = source.index(token)
        return {"byte_offset": len(source[:offset].encode()), "line": source[:offset].count("\n") + 1,
                "column": offset - source.rfind("\n", 0, offset)}

    headings = [(1, "small", "Small", "# Small"), (2, "details", "Details", "## Details")]
    doc = response["answer"]["report"]["document"]
    if "whole_get_requirements" not in manifest:
        expected = [{"level": level, "slug": slug, "text": text, "span": span(raw)}
                    for level, slug, text, raw in headings]
        if doc["headings"]["items"] != expected or any(doc[key]["items"] for key in
                                                     ("blocks", "tags", "links", "findings")):
            raise ValueError("whole get rich collections differ from the authored base fixture")
        return normalized
    requirements = manifest["whole_get_requirements"]
    if (requirements.get("target") != "small" or requirements.get("path") != "bench/small.md"
            or requirements.get("added_links") != 5 or requirements.get("required_tag") != "coldprobe"
            or requirements.get("missing_document") != "profilemissingcoldtarget"
            or requirements.get("missing_heading") != "ProfileMissingHeading"
            or requirements.get("missing_block") != "profilemissingblock"):
        raise ValueError("unsupported whole Get fixture requirements")
    headings.append((2, "linked-material", "Linked material", "## Linked material"))
    links = [("large", "[[large#Details]]", {"kind": "heading", "text": "Details"}, "bench/large.md", None),
             ("referrer-00", "[[referrer-00]]", None, "bench/referrer-00.md", None),
             ("profilemissingcoldtarget", "[[profilemissingcoldtarget]]", None, None, "target-missing"),
             ("large", "[[large#ProfileMissingHeading]]", {"kind": "heading", "text": "ProfileMissingHeading"}, "bench/large.md", "anchor-missing"),
             ("large", "[[large#^profilemissingblock]]", {"kind": "block", "id": "profilemissingblock"}, "bench/large.md", "block-ref-missing")]
    report = payload("current", response)
    doc = report['document']
    columns = {'headings': 3, 'blocks': 0, 'tags': 1, 'links': 5, 'findings': 3}
    if report.get('shape') != 'record' or any(k not in doc for k in columns):
        raise ValueError('whole get omitted a rich collection')
    for name, total in columns.items():
        if doc[name]['total'] != total or len(doc[name]['items']) != total:
            raise ValueError('whole get collection is truncated or has an incorrect total')
    if doc['tags']['items'] != [{'name': 'coldprobe', 'source': 'body', 'span': span('#coldprobe')}]:
        raise ValueError('whole get tag differs from authored answer')
    for actual, (target, raw, anchor, path, reason) in zip(doc['links']['items'], links):
        candidates = [{'path': path, 'suffix': target}] if path else []
        if (actual['target'] != target or actual['anchor'] != anchor or actual['span'] != span(raw)
                or actual['family'] != 'wikilink' or actual['embed'] or actual['protocol'] is not None
                or actual['title'] is not None or actual['health'] != ('healthy' if path else 'broken')
                or actual['targets'] != {'candidates': candidates, 'total': len(candidates)}):
            raise ValueError('whole get link answer differs from authored targets/anchors')
    findings = doc['findings']['items']
    for actual, (target, raw, anchor, path, reason) in zip(findings, links[2:]):
        expected_target = raw[2:-2]
        expected_kind = 'link/missing-anchor' if path else 'link/broken'
        candidates = [{'path': path, 'suffix': target}] if path else []
        if (actual['kind'] != expected_kind or actual['target'] != expected_target
                or actual['path'] != 'bench/small.md' or actual['span'] != span(raw)
                or actual['severity'] != 'warning' or actual['head'] != {'candidates': candidates, 'total': len(candidates)}):
            raise ValueError('whole get finding differs from authored violation or valid handle')
    actual_headings, span_key = doc['headings']['items'], 'span'
    expected_headings = [{'level': level, 'slug': slug, 'text': text, span_key: span(raw)}
                         for level, slug, text, raw in headings]
    if actual_headings != expected_headings:
        raise ValueError('get headings differ from authored answer')
    return normalized


def metadata_answer(stage):
    """Normalize only verified projection identity; preserve query content and cursor keys."""
    answer = copy.deepcopy(stage["answer"])
    report = answer["report"]

    def check(reading):
        if (reading["epoch"] != stage["projection_epoch"]
                or reading["generation"] != stage["projection_generation"]):
            raise ValueError("metadata projection provenance disagrees with its answer")

    if "counted_at" in report:
        check(report["counted_at"])
        report["counted_at"].pop("epoch")
        report["counted_at"].pop("generation")
    page = report.get("page", report)
    if page.get("next") is not None:
        encoded = page["next"]
        cursor = json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))
        check(cursor["snapshot"])
        cursor["snapshot"].pop("epoch")
        cursor["snapshot"].pop("generation")
        page["next"] = cursor
    return answer
