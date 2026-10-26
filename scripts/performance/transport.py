"""Isolated, persistent adapters. Only the supplied executable and run root are used."""

import json
import os
import queue
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path


TIMEOUT = 300
PREFIX = "NORN_PERFORMANCE "


def environment(root: Path, token: str) -> dict:
    env = {key: value for key, value in os.environ.items() if not key.startswith("NORN_")}
    for kind in ("CACHE", "STATE", "CONFIG", "DATA"):
        path = root / kind.lower()
        path.mkdir(exist_ok=True)
        env[f"XDG_{kind}_HOME"] = str(path)
    env.update(NORN_CACHE_CHANNEL="dev", NORN_PERFORMANCE_ROOT=str(root),
               NORN_PERFORMANCE_TOKEN=token)
    return env


def issue_root(root: Path) -> str:
    root.mkdir(parents=True, exist_ok=True)
    token = os.urandom(24).hex()
    (root / "performance-token").write_text(token)
    return token


def stop_process(process: subprocess.Popen) -> None:
    # Each process owns a new session; its descendants belong to this run too.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    # A leader can exit before a sweep, including one that ignores SIGTERM.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=10)
    until = time.monotonic() + 2
    while group_running(process.pid):
        if time.monotonic() >= until:
            raise RuntimeError("a subject descendant remains running after group cleanup")
        time.sleep(0.01)


def group_running(group: int) -> bool:
    for path in Path("/proc").glob("[0-9]*/stat"):
        try:
            fields = path.read_text().rsplit(")", 1)[1].split()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        if int(fields[2]) == group and fields[0] not in ("Z", "X"):
            return True
    return False


def resources(pid: int) -> dict:
    """Linux main-process readings; fixture generation and the controller are excluded."""
    proc = Path("/proc") / str(pid)
    result = {}
    for line in (proc / "status").read_text().splitlines():
        name, _, value = line.partition(":")
        if name in ("VmRSS", "VmHWM", "RssAnon", "RssFile", "RssShmem"):
            result[name + "_bytes"] = int(value.split()[0]) * 1024
        elif name == "Threads":
            result["threads"] = int(value)
    result["open_fds"] = len(list((proc / "fd").iterdir()))
    fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
    result["cpu_seconds"] = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    result["process_io"] = {name: int(value) for line in (proc / "io").read_text().splitlines()
                            for name, value in [line.split(":", 1)]}
    return result


class Current:
    def __init__(self, binary: Path, root: Path, *, changeset_size: int = 128, profiling: bool = False,
                 loading: str | None = None, stage_handler=None, loading_options: dict | None = None,
                 public_get_targets: list[str] | None = None, public_mid_request: dict | None = None):
        token = issue_root(root)
        env = environment(root, token)
        env.update(NORN_PERFORMANCE_CHANGESET_SIZE=str(changeset_size),
                   NORN_PERFORMANCE_PROFILE="1" if profiling else "0")
        if loading is not None:
            env["NORN_PERFORMANCE_LOADING"] = loading
        if loading_options is not None:
            if (loading not in ("four-step", "four-step-no-body")
                    or not {"transaction_bound", "requested_paths"} <= set(loading_options)
                    or set(loading_options) - {"transaction_bound", "requested_paths", "fts_mode", "mid_request_path"}
                    or loading_options.get("fts_mode", "rebuild") not in ("rebuild", "batched")
                    or ("mid_request_path" in loading_options and
                        (not isinstance(loading_options["mid_request_path"], str) or not loading_options["mid_request_path"]))
                    or type(loading_options["transaction_bound"]) is not int
                    or not 1 <= loading_options["transaction_bound"] <= 128
                    or not isinstance(loading_options["requested_paths"], list)
                    or any(not isinstance(path, str) for path in loading_options["requested_paths"])):
                raise ValueError("invalid four-step loading controls")
            env["NORN_PERFORMANCE_TRANSACTION_BOUND"] = str(loading_options["transaction_bound"])
            env["NORN_PERFORMANCE_REQUESTED_PATHS"] = json.dumps(loading_options["requested_paths"])
            env["NORN_PERFORMANCE_FTS_MODE"] = loading_options.get("fts_mode", "rebuild")
            if "mid_request_path" in loading_options:
                env["NORN_PERFORMANCE_MID_REQUEST_PATH"] = loading_options["mid_request_path"]
        if public_mid_request is not None:
            if (loading != "operator-prefix-public" or set(public_mid_request) != {"path", "offset_ns"}
                    or not isinstance(public_mid_request["path"], str) or not public_mid_request["path"]
                    or type(public_mid_request["offset_ns"]) is not int or public_mid_request["offset_ns"] < 0):
                raise ValueError("invalid public mid-request controls")
            env["NORN_PERFORMANCE_MID_REQUEST_PATH"] = public_mid_request["path"]
            env["NORN_PERFORMANCE_MID_REQUEST_OFFSET_NS"] = str(public_mid_request["offset_ns"])
        if public_get_targets is not None:
            if (loading != "operator-prefix-public" or not isinstance(public_get_targets, list)
                    or not public_get_targets
                    or any(not isinstance(target, str) or not target for target in public_get_targets)):
                raise ValueError("invalid public Get targets")
            env["NORN_PERFORMANCE_PUBLIC_GET_TARGETS"] = json.dumps(public_get_targets)
        self.stderr = (root / "worker.stderr.log").open("w", encoding="utf-8")
        self.raw = (root / "worker.stdout.log").open("w", encoding="utf-8")
        self.messages = queue.Queue()
        self.late_stage = False
        self.process = subprocess.Popen(
            [str(binary), "serve"],
            cwd=root, env=env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.stderr, text=True, encoding="utf-8", bufsize=1,
            start_new_session=True,
        )
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.stages = []
        try:
            self.ready = self.receive()
            while "startup_stage" in self.ready:
                stage = self.ready
                if stage_handler is not None:
                    stage_handler(stage)
                # Prefix handlers publish ACK before returning. Keep the sample at
                # this endpoint, but distinguish it from the paused validation sample.
                key = "resources_after_handler" if "operator_prefix_sequence" in stage else "resources_when_observed"
                stage[key] = resources(self.process.pid)
                self.stages.append(stage)
                self.ready = self.receive()
            if not self.ready.get("ready"):
                raise ValueError(f"current worker did not become ready: {self.ready}")
        except BaseException:
            self.close()
            raise

    def _read(self):
        ready_seen = False
        for line in self.process.stdout:
            self.raw.write(line)
            self.raw.flush()
            if PREFIX in line:
                message = json.loads(line.split(PREFIX, 1)[1])
                if ready_seen and "startup_stage" in message:
                    self.late_stage = True
                ready_seen |= message.get("ready") is True
                self.messages.put(message)
        self.messages.put(None)

    def receive(self):
        message = self.messages.get(timeout=TIMEOUT)
        if message is None:
            raise RuntimeError("current worker ended; inspect worker.stderr.log")
        return message

    def call(self, command: dict) -> tuple[dict, int]:
        request = json.dumps(command, separators=(",", ":")) + "\n"
        began = time.perf_counter_ns()
        self.process.stdin.write(request)
        self.process.stdin.flush()
        result = self.receive()
        return result, time.perf_counter_ns() - began

    def close(self):
        if self.process.poll() is None:
            try:
                self.process.stdin.write('{"op":"stop"}\n')
                self.process.stdin.flush()
                self.process.wait(timeout=10)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                pass
        stop_process(self.process)
        self.reader.join(timeout=5)
        self.raw.close()
        self.stderr.close()
        self.process.stdin.close()
        self.process.stdout.close()
        # Check the whole stream, including a checkpoint arriving after the final query.
        if self.late_stage and sys.exc_info()[0] is None:
            raise RuntimeError("startup checkpoint arrived after readiness; inspect worker logs")
        # Preserve the request failure that run.json records when cleanup follows it.
        if self.process.returncode != 0 and sys.exc_info()[0] is None:
            raise RuntimeError("current worker did not shut down cleanly; inspect worker logs")


class Legacy:
    def __init__(self, binary: Path, root: Path):
        token = issue_root(root)
        env = environment(root, token)
        at = root / "cache/norn/run/norn.sock"
        if len(os.fsencode(at)) >= 104:
            raise ValueError("use a shorter output path for the legacy Unix socket")
        self.stderr = (root / "worker.stderr.log").open("w")
        self.raw = (root / "worker.stdout.log").open("w")
        self.process = subprocess.Popen([str(binary), "serve"], cwd=root, env=env,
                                        stdout=self.raw, stderr=self.stderr,
                                        start_new_session=True)
        self.socket = None
        self.stream = None
        self.serial = 0
        try:
            until = time.monotonic() + TIMEOUT
            while not at.exists():
                if self.process.poll() is not None or time.monotonic() >= until:
                    raise RuntimeError("legacy daemon did not start; inspect worker.stderr.log")
                time.sleep(0.02)
            with socket.socket(socket.AF_UNIX) as ping:
                ping.settimeout(TIMEOUT)
                ping.connect(str(at))
                ping.sendall(b'{"norn_control":"ping","protocol":1}\n')
                with ping.makefile("rb") as response:
                    self.identity = json.loads(response.readline())
            if self.identity.get("norn_control") != "pong" or self.identity.get("pid") != self.process.pid:
                raise ValueError("the private socket did not identify the daemon started by this run")
            self.socket = socket.socket(socket.AF_UNIX)
            self.socket.settimeout(TIMEOUT)
            self.socket.connect(str(at))
            self.stream = self.socket.makefile("rb")
            began = time.perf_counter_ns()
            self.send({"norn_control": "hello", "protocol": 1,
                       "vault_root": str((root / "vault").resolve())})
            self.ready = self.receive()
            self.ready["attach_ns"] = time.perf_counter_ns() - began
            if self.ready.get("norn_control") != "ready":
                raise ValueError(f"legacy vault did not become ready: {self.ready}")
            self.rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                    "clientInfo": {"name": "norn-performance", "version": "1"}})
            self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        except BaseException:
            self.close()
            raise

    def send(self, value):
        self.socket.sendall((json.dumps(value, separators=(",", ":")) + "\n").encode())

    def receive(self):
        line = self.stream.readline()
        if not line:
            raise RuntimeError("legacy connection ended; inspect worker.stderr.log")
        return json.loads(line)

    def rpc(self, method, params):
        self.serial += 1
        self.send({"jsonrpc": "2.0", "id": self.serial, "method": method, "params": params})
        while True:
            response = self.receive()
            if response.get("id") == self.serial:
                if "error" in response:
                    raise ValueError(f"legacy JSON-RPC failure: {response['error']}")
                return response["result"]

    def call(self, command: dict) -> tuple[dict, int]:
        op = command["op"]
        if op == "search":
            tool, args = "vault.find", {"text": command["query"], "sort": "path",
                                       "limit": 25, "col": [".path"]}
            if "path" in command:
                args["path"] = [command["path"]]
        elif op == "find":
            tool, args = "vault.find", {"eq": ["perf_bucket:active"], "sort": "path",
                                       "limit": 25, "col": [".path"]}
        elif op == "get":
            target, _, section = command["target"].partition("#")
            tool, args = "vault.get", {"targets": [target]}
            if section:
                args["section"] = [section]
            else:
                args["all_cols"] = True
        elif op == "set":
            tool, args = "vault.set", {"target": "bench/subject.md", "confirm": True,
                                      "field_json": ["status=" + json.dumps(command["value"])]}
        elif op == "new":
            tool, args = "vault.new", {"path": command["path"], "confirm": True,
                                      "field_json": ['status="draft"'], "body": command["body"]}
        elif op == "move":
            tool, args = "vault.move", {"from": command["from"], "to": command["to"],
                                       "confirm": True}
        else:
            raise ValueError(f"unknown operation {op}")
        began = time.perf_counter_ns()
        result = self.rpc("tools/call", {"name": tool, "arguments": args})
        elapsed = time.perf_counter_ns() - began
        if result.get("isError"):
            raise ValueError(f"legacy tool failed: {result}")
        return result, elapsed

    def close(self):
        if self.stream is not None:
            self.stream.close()
        if self.socket is not None:
            self.socket.close()
        stop_process(self.process)
        self.raw.close()
        self.stderr.close()
