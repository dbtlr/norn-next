"""Isolated, persistent adapters. Only the supplied executable and run root are used."""

import json
import os
import queue
import signal
import socket
import subprocess
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
        if name in ("VmRSS", "VmHWM"):
            result[name + "_bytes"] = int(value.split()[0]) * 1024
        elif name == "Threads":
            result["threads"] = int(value)
    result["open_fds"] = len(list((proc / "fd").iterdir()))
    fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
    result["cpu_seconds"] = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    return result


class Current:
    def __init__(self, binary: Path, root: Path):
        token = issue_root(root)
        self.stderr = (root / "worker.stderr.log").open("w")
        self.raw = (root / "worker.stdout.log").open("w")
        self.messages = queue.Queue()
        self.process = subprocess.Popen(
            [str(binary), "serve"],
            cwd=root, env=environment(root, token), stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.stderr, text=True, bufsize=1,
            start_new_session=True,
        )
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        try:
            self.ready = self.receive()
            if not self.ready.get("ready"):
                raise ValueError(f"current worker did not become ready: {self.ready}")
        except BaseException:
            self.close()
            raise

    def _read(self):
        for line in self.process.stdout:
            if PREFIX in line:
                self.messages.put(json.loads(line.split(PREFIX, 1)[1]))
            else:
                self.raw.write(line)
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
        if self.process.returncode != 0:
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
