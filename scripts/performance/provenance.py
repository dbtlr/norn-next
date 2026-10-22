"""Build the current worker with the repository compiler and retain Cargo's profile evidence."""

import argparse
import hashlib
import json
import os
import subprocess
import tomllib
from pathlib import Path


def sha256(path: Path) -> str:
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def source_tree_digest(repo: Path) -> str:
    names = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=repo)
    digest = hashlib.sha256()
    for name in sorted(set(names.split(b"\0")) - {b""}):
        path = repo / os.fsdecode(name)
        if path.is_symlink():
            data = b"link:" + os.fsencode(os.readlink(path))
        else:
            data = b"file:" + path.read_bytes() if path.is_file() else b"missing"
        digest.update(len(name).to_bytes(8, "big") + name)
        digest.update(len(data).to_bytes(8, "big") + data)
    return digest.hexdigest()


def verify_build(path: Path, repo: Path | None = None) -> dict:
    receipt = json.loads(path.read_text())
    if receipt["profile"]["opt_level"] != "3" or receipt["profile"]["debug_assertions"]:
        raise ValueError("the baseline requires Cargo's optimized release profile")
    if sha256(Path(receipt["executable"])) != receipt["sha256"]:
        raise ValueError("the executable differs from its build receipt")
    repo = repo or Path(__file__).resolve().parents[2]
    if source_tree_digest(repo) != receipt["source_tree_sha256"]:
        raise ValueError("the source tree differs from the build receipt; rebuild in this checkout")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-directory", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    toolchain = tomllib.loads((repo / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    compiler = subprocess.check_output(["rustup", "which", "--toolchain", toolchain, "rustc"], text=True).strip()
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("CARGO_PROFILE_") and key not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")}
    env["CARGO_TARGET_DIR"] = str(args.target_directory.resolve())
    # These highest-precedence settings also override target/config rustflags and compiler wrappers.
    env.update(CARGO_ENCODED_RUSTFLAGS="", RUSTC=compiler, RUSTC_WRAPPER="", RUSTC_WORKSPACE_WRAPPER="")
    command = ["rustup", "run", toolchain, "cargo", "test", "--locked", "--release", "-p",
               "norn-host", "--test", "performance", "--no-run", "--message-format=json"]
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    source_hash = source_tree_digest(repo)
    result = subprocess.run(command, cwd=repo, env=env, text=True, capture_output=True)
    args.receipt.with_suffix(".cargo.jsonl").write_text(result.stdout)
    args.receipt.with_suffix(".cargo.log").write_text(result.stderr)
    result.check_returncode()
    if source_tree_digest(repo) != source_hash:
        raise ValueError("the source tree changed during compilation")
    artifacts = [json.loads(line) for line in result.stdout.splitlines()]
    artifact = next(item for item in artifacts if item.get("reason") == "compiler-artifact"
                    and item["target"]["name"] == "performance" and item.get("executable"))
    receipt = {"executable": artifact["executable"], "sha256": sha256(Path(artifact["executable"])),
               "profile": artifact["profile"], "features": artifact["features"], "command": command,
               "rustc": subprocess.check_output([compiler, "-vV"], text=True),
               "effective_compiler": compiler, "encoded_rustflags": "", "compiler_wrappers": "disabled",
               "cargo": subprocess.check_output(["rustup", "run", toolchain, "cargo", "--version"], text=True),
               "repository_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
               "repository_status": subprocess.check_output(["git", "status", "--porcelain"], cwd=repo, text=True),
               "worker_source_sha256": sha256(repo / "crates/norn-host/tests/performance.rs"),
               "cargo_lock_sha256": sha256(repo / "Cargo.lock"), "source_tree_sha256": source_hash,
               "workspace_library_features": {item["target"]["name"]: item["features"] for item in artifacts
                   if item.get("reason") == "compiler-artifact" and "lib" in item["target"]["kind"]
                   and item["target"]["name"].startswith("norn")}}
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    verify_build(args.receipt)
    print(args.receipt)


if __name__ == "__main__":
    main()
