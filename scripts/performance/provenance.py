"""Build the current worker with the repository compiler and retain Cargo's profile evidence."""

import argparse
import hashlib
import json
import os
import subprocess
import tomllib
from pathlib import Path

from transport import stop_process


def run_build(command: list[str], repo: Path, env: dict, receipt: Path,
              timeout: float = 1800) -> str:
    """Keep live build diagnostics, including when the isolated build times out."""
    output = receipt.with_suffix(".cargo.jsonl")
    errors = receipt.with_suffix(".cargo.log")
    with output.open("w", encoding="utf-8") as stdout, errors.open("w", encoding="utf-8") as stderr:
        process = subprocess.Popen(command, cwd=repo, env=env, stdout=stdout, stderr=stderr,
                                   start_new_session=True)
        try:
            returncode = process.wait(timeout=timeout)
            if returncode:
                raise subprocess.CalledProcessError(returncode, command)
        finally:
            stop_process(process)
    return output.read_text(encoding="utf-8")


def sha256(path: Path) -> str:
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def source_tree_digest(repo: Path) -> str:
    names = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", ".", ":(top,exclude).t"], cwd=repo, timeout=30)
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


def cargo_configs(repo: Path) -> list[dict]:
    folders = [parent / ".cargo" for parent in (repo, *repo.parents)]
    folders.append(Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))))
    paths = {folder / name for folder in folders for name in ("config", "config.toml")}
    return [{"path": str(path), "sha256": sha256(path)} for path in sorted(paths) if path.is_file()]


def verify_build(path: Path, repo: Path | None = None) -> dict:
    receipt = json.loads(path.read_text())
    if receipt["profile"]["opt_level"] != "3" or receipt["profile"]["debug_assertions"]:
        raise ValueError("the baseline requires Cargo's optimized release profile")
    if sha256(Path(receipt["executable"])) != receipt["sha256"]:
        raise ValueError("the executable differs from its build receipt")
    repo = repo or Path(__file__).resolve().parents[2]
    if source_tree_digest(repo) != receipt["source_tree_sha256"]:
        raise ValueError("the source tree differs from the build receipt; rebuild in this checkout")
    if cargo_configs(repo) != receipt.get("cargo_configs", []):
        raise ValueError("Cargo configuration differs from the build receipt")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-directory", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--startup-profile", action="store_true",
                        help="include the host's harness-only startup timing and work readers")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    outputs = (args.target_directory, args.receipt, args.receipt.with_suffix(".cargo.jsonl"),
               args.receipt.with_suffix(".cargo.log"))
    if any(path.resolve().is_relative_to(repo) for path in outputs):
        raise ValueError("build outputs must resolve outside the repository; choose another target directory and receipt path")
    toolchain = tomllib.loads((repo / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    compiler = subprocess.check_output(["rustup", "which", "--toolchain", toolchain, "rustc"], text=True, timeout=30).strip()
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("CARGO_PROFILE_", "CARGO_BUILD_"))
           and key not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_INCREMENTAL", "RUSTC_FORCE_INCREMENTAL")}
    env["CARGO_TARGET_DIR"] = str(args.target_directory.resolve())
    # These highest-precedence settings also override target/config rustflags and compiler wrappers.
    env.update(CARGO_ENCODED_RUSTFLAGS="", RUSTC=compiler, RUSTC_WRAPPER="", RUSTC_WORKSPACE_WRAPPER="")
    command = ["rustup", "run", toolchain, "cargo", "test", "--locked", "--release", "-p",
               "norn-host", "--test", "performance", "--no-run", "--message-format=json"]
    if args.startup_profile:
        command.extend(["--features", "induced-failure"])
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    source_hash = source_tree_digest(repo)
    configs = cargo_configs(repo)
    output = run_build(command, repo, env, args.receipt)
    if source_tree_digest(repo) != source_hash or cargo_configs(repo) != configs:
        raise ValueError("source or Cargo configuration changed during compilation")
    artifacts = [json.loads(line) for line in output.splitlines()]
    artifact = next(item for item in artifacts if item.get("reason") == "compiler-artifact"
                    and item["target"]["name"] == "performance" and item.get("executable"))
    receipt = {"executable": artifact["executable"], "sha256": sha256(Path(artifact["executable"])),
               "profile": artifact["profile"], "features": artifact["features"], "command": command,
               "rustc": subprocess.check_output([compiler, "-vV"], text=True, timeout=30),
               "effective_compiler": compiler, "encoded_rustflags": "", "compiler_wrappers": "disabled",
               "cargo": subprocess.check_output(["rustup", "run", toolchain, "cargo", "--version"], text=True, timeout=30),
               "repository_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True, timeout=30).strip(),
               "repository_status": subprocess.check_output(["git", "status", "--porcelain"], cwd=repo, text=True, timeout=30),
               "worker_source_sha256": sha256(repo / "crates/norn-host/tests/performance.rs"),
               "cargo_lock_sha256": sha256(repo / "Cargo.lock"), "source_tree_sha256": source_hash,
               "cargo_configs": configs,
               "workspace_library_features": {item["target"]["name"]: item["features"] for item in artifacts
                   if item.get("reason") == "compiler-artifact" and "lib" in item["target"]["kind"]
                   and item["target"]["name"].startswith("norn")}}
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    verify_build(args.receipt)
    print(args.receipt)


if __name__ == "__main__":
    main()
