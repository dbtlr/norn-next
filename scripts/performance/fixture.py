"""Known-answer probes planted in a tree produced by norn-fixtures."""

import hashlib
from pathlib import Path


COMMON = "profilecommonword"
RARE = "profileraresignal"
MISSING = "profilenonexistenttoken"
SECTION = "## Details\nA bounded section with a precise answer.\n"


def tree_digest(root: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("performance fixtures must not contain symlinks")
        if path.is_file():
            name = path.relative_to(root).as_posix().encode()
            data = path.read_bytes()
            digest.update(len(name).to_bytes(8, "big") + name)
            digest.update(len(data).to_bytes(8, "big") + data)
    return digest.hexdigest()


def prepare_probes(root: Path) -> dict:
    # Reject links before modifying any generated document.
    tree_digest(root)
    active = []
    background = sorted(root.rglob("*.md"))
    for number, path in enumerate(background):
        text = path.read_text()
        if COMMON in text or RARE in text or MISSING in text:
            raise ValueError("probe token collides with the generated background")
        bucket = "active" if number % 2 == 0 else "archived"
        if not text.startswith("---\n"):
            raise ValueError("generated documents must have frontmatter")
        text = "---\nperf_bucket: " + bucket + "\n" + text[4:]
        path.write_text(text + "\n" + COMMON + "\n")
        if bucket == "active":
            active.append(path.relative_to(root).as_posix())

    small = "# Small\n" + RARE + " " + COMMON + "\n"
    small += "A regular document with a few paragraphs of stable reference material.\n" * 30
    small += SECTION
    large = "# Large\n" + ("A deliberately long document body with stable content.\n" * 900)
    large += COMMON + "\n" + SECTION + "## Tail\nEnd of the document.\n"
    probes = {
        "bench/small.md": small,
        "bench/large.md": large,
        "bench/subject.md": "# Subject\n" + COMMON + "\n",
        "bench/hub.md": "# Hub\n" + COMMON + "\n",
    }
    for number in range(10):
        probes[f"bench/referrer-{number:02}.md"] = (
            "# Referrer\n[[bench/hub]]\n" + COMMON + "\n"
        )
    for name, body in probes.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("---\nstatus: draft\nperf_bucket: probe\n---\n" + body)

    control = root / ".norn"
    control.mkdir(exist_ok=True)
    (control / "schema.yaml").write_text("version: 1\n")
    paths = sorted(p.relative_to(root).as_posix() for p in root.rglob("*.md"))
    return {
        "workload_version": "runtime-baseline-v1",
        "background_count": len(background),
        "document_count": len(paths),
        "paths": paths,
        "rare_paths": ["bench/small.md"],
        "active_paths": active,
        "filtered_paths": sorted(probes),
        "probe_bodies": probes,
        "small_body": small,
        "large_body": large,
        "section": SECTION,
        "tree_sha256": tree_digest(root),
        "markdown_bytes": sum((root / name).stat().st_size for name in paths),
    }
