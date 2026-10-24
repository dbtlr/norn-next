"""Validate frozen source against a store-owned, read-only audit worker.

SQLite inspection and semantic normalization belong to norn-store. This
controller checks the worker's ordered document triples against source bytes;
it never opens the derived database. Run only after the measured worker closes.
"""

import hashlib
import json
import os
import selectors
import subprocess
import time
from pathlib import Path

from fixture import tree_digest
from transport import PREFIX, TIMEOUT, environment, stop_process


def messages(process, raw, deadline):
    """Stream bounded JSONL chunks, retaining partial output on failure."""
    pending = b""
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise TimeoutError("startup audit worker exceeded its deadline")
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                if pending:
                    raise ValueError("startup audit worker ended with a partial record")
                return
            raw.write(chunk)
            raw.flush()
            pending += chunk
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                text = line.decode("utf-8")
                if not text.startswith(PREFIX):
                    raise ValueError("startup audit worker emitted an unexpected record")
                yield json.loads(text[len(PREFIX):])
            if len(pending) > 1024 * 1024:
                raise ValueError("startup audit worker emitted an oversized record")


def audit_store(binary: Path, run_root: Path, manifest: dict) -> dict:
    """Audit persisted semantics and source bytes outside measurement clocks."""
    stores = list((run_root / "data").rglob("store.sqlite3"))
    if len(stores) != 1 or not stores[0].is_file():
        raise ValueError("startup audit requires exactly one store.sqlite3 below data")
    vault = run_root / "vault"
    source_sha256 = tree_digest(vault)
    if source_sha256 != manifest["tree_sha256"]:
        raise ValueError("frozen vault no longer matches its manifest")
    paths = sorted(path.relative_to(vault).as_posix() for path in vault.rglob("*.md"))
    if paths != manifest["paths"] or len(paths) != manifest["document_count"]:
        raise ValueError("frozen Markdown paths differ from the manifest")
    token = (run_root / "performance-token").read_text(encoding="utf-8")
    env = environment(run_root, token)
    count, byte_length, summary = 0, 0, None
    deadline = time.monotonic() + TIMEOUT
    # Append so a later warm audit cannot erase cold or partial evidence.
    with (run_root / "audit.stdout.log").open("ab") as raw, \
            (run_root / "audit.stderr.log").open("ab") as errors:
        process = subprocess.Popen(
            [str(binary), "audit"], cwd=run_root, env=env, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=errors, bufsize=0, start_new_session=True,
        )
        try:
            for record in messages(process, raw, deadline):
                if not isinstance(record, dict) or summary is not None:
                    raise ValueError("startup audit worker emitted an unexpected record")
                if set(record) == {"document"}:
                    document = record["document"]
                    if count >= len(paths) or document["path"] != paths[count]:
                        raise ValueError("stored document paths differ from the frozen vault")
                    content = (vault / paths[count]).read_bytes()
                    if document["content_hash"] != hashlib.sha256(content).hexdigest() \
                            or document["byte_length"] != len(content):
                        raise ValueError(f"stored document hash or byte length differs from source: {paths[count]}")
                    count += 1
                    byte_length += len(content)
                elif set(record) == {"summary"}:
                    summary = record["summary"]
                else:
                    raise ValueError("startup audit worker emitted an unexpected record")
            process.wait(timeout=max(0.001, deadline - time.monotonic()))
            if process.returncode != 0:
                raise RuntimeError("startup audit worker failed; inspect audit.stderr.log")
        finally:
            stop_process(process)
            process.stdout.close()
    if count != len(paths):
        raise ValueError("stored document paths differ from the frozen vault")
    if byte_length != manifest["markdown_bytes"]:
        raise ValueError("frozen Markdown byte length differs from the manifest")
    if not isinstance(summary, dict) or summary.get("schema_version") != 3:
        raise ValueError("startup audit worker supplied no schema-3 summary")
    construction = summary.get("document_construction_sha256")
    if (not isinstance(construction, str) or len(construction) != 64
            or any(char not in "0123456789abcdef" for char in construction)):
        raise ValueError("startup audit worker supplied no valid document construction digest")
    if summary["counts"]["documents"] != count or summary["tables"]["documents"]["rows"] != count:
        raise ValueError("startup audit document counts disagree")
    return summary | {"source_sha256": source_sha256}
