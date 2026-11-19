"""The real HTTP serving subject, isolated from every resident channel."""

import hashlib
import hmac
import http.client
import json
import os
import queue
import random
import secrets
import select
import signal
import subprocess
import sys
import threading
import time
import tomllib
from pathlib import Path

from transport import TIMEOUT, environment, issue_root, stop_process


# Release calibration at 2k/10k: the largest small-report p95 was 4.765 ms.
# Get's observed extra cost per added byte was 30.2/100.7 ns at those scales.
# Round those up with room for scheduling and transport variation.
OVERHEAD_FIXED_NS = 6_000_000
OVERHEAD_PER_BYTE_NS = 125

# The MCP delta allowance is 1 ms plus 10 ns per report byte. It is set from
# the first hosted release record (run 44497618284, 400 samples per
# route/workload at 2,014 and 10,014 documents, 3 processes each): the largest
# small-report median delta was 0.034 ms and Get large (51,481 report bytes)
# added 0.084 ms, about 1.6 ns per byte. The fixed term is about 30 times the
# small-report reading and the per-byte term about 6 times the large one, so
# scheduling noise on a hosted runner stays inside it. An added millisecond
# exceeds the allowance only on a report under about 3.4 KB: past that, the
# 10 ns per byte the allowance grants outweighs the 0.034 ms baseline.
MCP_DELTA_FIXED_NS = 1_000_000
MCP_DELTA_PER_BYTE_NS = 10


ADDRESS = {"by": "name", "name": "notes"}
# The counters route's gauges: readings at an instant, with no delta.
GAUGES = frozenset({"serve.alive_tasks"})

# The compiled development channel, which the isolated subject runs as.
CHANNEL = "dev"
HELLO_DOMAIN = b"norn-hello-v1\x00"
HELLO_FIELDS = ("channel", "build", "launch", "state", "host", "client")


def socket_address(ip, port):
    """An address as the host writes it: `ip:port`, IPv6 as `[ip]:port`."""
    return f"[{ip}]:{port}" if ":" in ip else f"{ip}:{port}"


def hello_mac_input(nonce, facts):
    """The bytes the hello's MAC covers, as `norn-wire`'s `mac_input` builds them.

    The domain with one zero byte, then the raw nonce and the channel, build,
    launch, state, host and client strings, each behind a 4-byte big-endian
    length.
    """
    out = HELLO_DOMAIN
    for field in (nonce, *(facts[name].encode() for name in HELLO_FIELDS)):
        out += len(field).to_bytes(4, "big") + field
    return out


def hello_mac(secret, nonce, facts):
    """The MAC as the answer carries it: HMAC-SHA256 as lowercase hex."""
    return hmac.new(secret, hello_mac_input(nonce, facts), hashlib.sha256).hexdigest()


def verify_hello(status, answer, *, nonce, secret, channel, host, client):
    """Raise naming the first failed check; return only for a host that proved itself (ADR 0054)."""
    if status != 200:
        raise ValueError(f"hello status {status} is not 200")
    if not isinstance(answer, dict) or answer.get("version") != 1:
        raise ValueError(f"hello version is not 1: {answer!r}")
    mac = answer.get("mac")
    if not isinstance(mac, str) or not all(isinstance(answer.get(name), str) for name in HELLO_FIELDS):
        raise ValueError("hello mac or facts are missing or not text")
    # The MAC comes first: until it verifies, the facts are the answerer's word.
    if not hmac.compare_digest(hello_mac(secret, nonce, answer).encode(), mac.encode()):
        raise ValueError("hello mac does not verify under the token")
    for name, expected in (("channel", channel), ("host", host), ("client", client)):
        if answer[name] != expected:
            raise ValueError(f"hello {name} {answer[name]!r} is not the expected {expected!r}")


def params(command):
    """Translate the frozen workload to native wire params, without verb logic."""
    op = command["op"]
    body = {"vault": ADDRESS}
    if op == "get":
        body.update(target=command["target"], columns=[], collection=None, limit=None, after=None)
    elif op in ("find", "search"):
        body.update(predicates=[], columns=[], limit=25, after=None)
        if op == "find":
            body.update(sort=None, predicates=[{"op": "eq", "key": "perf_bucket", "value": "active"}])
        else:
            body.update(query=command["query"], rungs={"select": "exactly", "rungs": ["lexical"]}, min_score=None)
            if "path" in command:
                body["predicates"] = [{"op": "path", "glob": command["path"]}]
    elif op == "set":
        body.update(mode="apply", path="bench/subject.md", changes=[{"change": "set", "field": "status", "value": command["value"]}])
    elif op == "new":
        body.update(mode="apply", path=command["path"], content=command["content"])
    elif op == "move":
        body.update(mode="apply", **{"from": command["from"], "to": command["to"]})
    else:
        raise ValueError(f"unknown serving workload {op}")
    return op, body


def other_servers(proc=Path("/proc")):
    """Include real resident `norn serve` processes in the quiet-host evidence."""
    found = []
    for path in proc.glob("[0-9]*/cmdline"):
        try:
            argv = path.read_bytes().split(b"\0")
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            continue
        executable = Path(os.fsdecode(argv[0]))
        is_subject = (executable.name == "norn" or executable.name.startswith("norn-")
                      or executable.parent.name == "binaries" and executable.name in ("serve", "current", "legacy"))
        if len(argv) > 1 and is_subject and argv[1] == b"serve":
            found.append(int(path.parent.name))
    return sorted(found)


# Each running subject's pid and kernel start time, beside its logs, from its
# launch until it is reaped. The subject runs in a session of its own and
# outlives a recorder killed before it could close it; a controller stops it
# from this note.
SUBJECT_NOTE = "subject.json"


def start_time(pid, proc=Path("/proc")):
    """When `pid` started, in clock ticks since boot: a reused pid does not share it."""
    stat = (proc / str(pid) / "stat").read_text()
    return int(stat.rsplit(")", 1)[1].split()[19])


def running(pid, proc=Path("/proc")):
    """Whether `pid` exists and has not exited; a zombie has."""
    try:
        state = (proc / str(pid) / "stat").read_text().rsplit(")", 1)[1].split()[0]
    except (FileNotFoundError, ProcessLookupError):
        return False
    return state not in ("Z", "X")


def note_subject(path, pid):
    """Note `pid` and its start time at `path`, whole or not at all: written beside it, then renamed."""
    partial = path.with_name(path.name + ".partial")
    partial.write_text(json.dumps({"pid": pid, "start_time": start_time(pid)}))
    os.replace(partial, path)


def signal_group(pid, number):
    try:
        os.killpg(pid, number)
    except ProcessLookupError:
        pass


def group_alive(pgid):
    """Whether any process is still in the process group `pgid`."""
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def stop_group(pgid, grace=10, reap=None):
    """Stop every process left in the group `pgid`: SIGTERM, then SIGKILL past `grace` seconds.

    `reap` reaps the group's leader where the caller owns it (`Popen.poll`): an
    exited leader the caller has not reaped is a zombie that still holds the
    group, so without it the wait would run out its grace for nothing.

    A group id stays taken while any member lives, so a group found alive here
    is the one the caller made. The one window left is a group whose last
    member exits between the liveness check and the signal, and whose id a new
    group leader takes in that instant; it is accepted.
    """
    for number, wait in ((signal.SIGTERM, grace), (signal.SIGKILL, 5)):
        if not group_alive(pgid):
            return
        signal_group(pgid, number)
        deadline = time.monotonic() + wait
        while time.monotonic() < deadline:
            if reap is not None:
                reap()
            if not group_alive(pgid):
                return
            time.sleep(0.05)


def pinned(pid, noted_start):
    """A pidfd for `pid` if it is still the process noted, or None.

    The pidfd is opened before the start time is checked, so once the check
    passes the descriptor names that process for good: signals sent through it
    can never reach a process that later reuses the pid.
    """
    try:
        descriptor = os.pidfd_open(pid)
    except (ProcessLookupError, OSError):
        return None
    try:
        if start_time(pid) == noted_start and running(pid):
            return descriptor
    except (FileNotFoundError, ProcessLookupError):
        pass
    os.close(descriptor)
    return None


def exited_within(descriptor, seconds):
    """Whether the process `descriptor` pins exits within `seconds`."""
    poller = select.poll()
    poller.register(descriptor, select.POLLIN)
    return bool(poller.poll(seconds * 1000))


def stop_subjects(directory, grace=10):
    """Stop every subject a note under `directory` names that still runs as noted, and drop the notes.

    The subject is signalled through a pidfd pinned to the noted process
    (SIGTERM, then SIGKILL past `grace` seconds), so a reused pid is never
    signalled. A `norn serve` subject starts no children, so the process is the
    whole of it. A note that does not read (a write cut short) names no process
    to stop: it is reported and dropped, and the notes after it are still
    served. Answers the pids stopped.
    """
    stopped = []
    for note_path in sorted(Path(directory).rglob(SUBJECT_NOTE)):
        try:
            note = json.loads(note_path.read_text())
            pid, noted_start = int(note["pid"]), int(note["start_time"])
        except (ValueError, KeyError, TypeError, OSError) as error:
            print(f"subject note {note_path} does not read ({error}); nothing to stop", file=sys.stderr)
            note_path.unlink(missing_ok=True)
            continue
        descriptor = pinned(pid, noted_start)
        if descriptor is not None:
            try:
                for number, wait in ((signal.SIGTERM, grace), (signal.SIGKILL, 5)):
                    try:
                        signal.pidfd_send_signal(descriptor, number)
                    except ProcessLookupError:
                        break
                    if exited_within(descriptor, wait):
                        break
            finally:
                os.close(descriptor)
            stopped.append(pid)
        note_path.unlink(missing_ok=True)
    return stopped

def subject_config(port):
    """The channel's `config.toml` for the isolated subject.

    The subject starts as the CLI launches a host, so the first-answer clock is
    measured after an auto-launch. `--auto-launched` conflicts with `--port`,
    so the port is the config's `[endpoint] port`. An auto-launched host exits
    after `auto_exit_after` without a request, and the idle-detach boundary
    waits for a long quiet spell; the setting is held at its 24 h maximum so
    the host outlives the run.
    """
    return f'[endpoint]\nport = {port}\n[serve]\nidle_after = "1m"\nauto_exit_after = "24h"\n'


def launch_command(binary):
    """The argv that starts the subject as an auto-launched host."""
    return [str(binary), "serve", "--auto-launched"]


class Serve:
    def __init__(self, binary, root):
        token = issue_root(root)
        env = environment(root, token)
        # The compiled development channel's directories are private to this run.
        config = root / "config/norn-dev"
        config.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.port = self.free_port()
        (config / "config.toml").write_text(subject_config(self.port))
        # The launch measures a configured vault, before any derived state exists.
        registry = config / "registry.toml"
        registry.write_text('version = 1\n[vaults.notes]\nroot = ' + json.dumps(str((root / "vault").resolve())) + '\n')
        registrations = len(tomllib.loads(registry.read_text())["vaults"])
        if list((root / "data").iterdir()):
            raise ValueError("the serving subject requires a fresh derived-data directory")
        self.stderr = (root / "worker.stderr.log").open("w")
        self.stdout = (root / "worker.stdout.log").open("w")
        self.attempts = 0
        self.hellos = 0
        self.connection = None
        self.route = "wire"
        self.mcp_initialized = False
        self.channel = CHANNEL
        self.first_use = []
        lines = queue.Queue()
        began = time.perf_counter_ns()
        self.process = subprocess.Popen(launch_command(binary), cwd=root,
                                        env=env, stdout=self.stdout, stderr=subprocess.PIPE,
                                        text=True, start_new_session=True)
        def read_lines():
            for line in self.process.stderr:
                self.stderr.write(line)
                self.stderr.flush()
                lines.put(line)
            lines.put(None)
        self.note = root / SUBJECT_NOTE
        self.reader = threading.Thread(target=read_lines, daemon=True)
        self.reader.start()
        try:
            note_subject(self.note, self.process.pid)
            deadline = time.monotonic() + 30
            while True:
                line = lines.get(timeout=max(0.001, deadline - time.monotonic()))
                if line is None:
                    raise RuntimeError("serve exited before listening; inspect worker.stderr.log")
                if line.startswith("listening:"):
                    break
            self.first_use.append({"boundary": "launch_to_listening", "elapsed_ns": time.perf_counter_ns() - began,
                                   "registrations": registrations, "derived_directories": 0, "hellos": 0, "connection_attempts": 0, "token_lookups": 0})
            files = list((root / "config").rglob("tokens.toml"))
            if len(files) != 1:
                raise ValueError("serve did not create exactly one isolated token file")
            self.token_file = files[0]
            self.bearer = tomllib.loads(files[0].read_text())["tokens"]["default"]["secret"]
            # The token file holds the secret as lowercase hex; the MAC key is its raw bytes.
            self.secret = bytes.fromhex(self.bearer)
            # The first answer's clock includes this verified hello, as it always did.
            self.connect()
            first = self.boundary("first_answer_after_auto_launch", since=began)
            self.wait_state("ready", 180)
            # Match T3's settled-file bar: the production fingerprint is
            # trusted after 3 s, with 200 ms for the filesystem clock tick.
            while True:
                metadata = self.token_file.stat()
                remaining = max(metadata.st_mtime_ns, metadata.st_ctime_ns) + 3_200_000_000 - time.time_ns()
                if remaining <= 0:
                    break
                time.sleep(min(remaining / 1e9, 0.05))
            self.ready = {"first_use": self.first_use, "first_answer": first}
        except BaseException:
            self.close()
            raise

    @staticmethod
    def free_port():
        import socket
        for port in random.sample(range(20000, 30000), 1000):
            with socket.socket() as listener:
                try:
                    listener.bind(("127.0.0.1", port))
                    return port
                except OSError:
                    continue
        raise RuntimeError("no free serving benchmark port")

    @staticmethod
    def delta(before, after):
        return {key: value - before[key] for key, value in after.items() if key not in GAUGES}

    def close_connection(self):
        if self.connection is not None:
            self.connection.close()
            self.connection = None

    def connect(self):
        """Open a connection and verify its host before any bearer can use it (ADR 0054).

        The only way a connection comes to exist, so no caller can skip the
        hello. A host that fails any check gets no bearer and the connection
        is closed.
        """
        self.connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=TIMEOUT)
        # http.client reopens a closed socket by itself on the next request.
        # A reopened socket is a new connection whose listener signed no hello,
        # so reopening is turned off and request() opens every socket here.
        self.connection.auto_open = 0
        self.attempts += 1
        try:
            self.connection.connect()
            local = self.connection.sock.getsockname()
            nonce = secrets.token_bytes(32)
            body = {"label": "default", "nonce": nonce.hex()}
            answer, _, _, _, status = self.exchange("POST", "/v1/hello", body, auth=False)
            verify_hello(status, answer, nonce=nonce, secret=self.secret, channel=self.channel,
                         host=socket_address("127.0.0.1", self.port), client=socket_address(*local[:2]))
        except BaseException:
            self.close_connection()
            raise
        self.mcp_initialized = False
        self.hellos += 1

    def exchange(self, method, path, body, *, auth):
        """One request on the open connection: answer, elapsed, host time, size, status."""
        headers = {"Content-Type": "application/json"}
        if auth:
            headers["Authorization"] = "Bearer " + self.bearer
        if path == "/mcp":
            revision = body.get("params", {}).get("_meta", {}).get("io.modelcontextprotocol/protocolVersion", "2025-11-25")
            headers["MCP-Protocol-Version"] = revision
            if revision == "2026-07-28":
                headers["Mcp-Method"] = body["method"]
        encoded = json.dumps(body).encode() if body is not None else b""
        began = time.perf_counter_ns()
        self.connection.request(method, path, encoded, headers)
        response = self.connection.getresponse()
        data = response.read()
        elapsed = time.perf_counter_ns() - began
        try:
            answer = json.loads(data)
        except ValueError:
            answer = data
        return answer, elapsed, response.getheader("x-norn-host-time-us"), len(data), response.status

    def request(self, method, path, body=None):
        # A host that answered with `Connection: close` left the connection
        # without a socket; the next socket is verified like the first.
        if self.connection is None or self.connection.sock is None:
            self.close_connection()
            self.connect()
            if self.connection.sock is None:
                self.close_connection()
                raise ValueError("hello closed: the host closed the connection with its hello "
                                 "answer, so no verified connection is left to carry the bearer")
        answer, elapsed, host_us, size, status = self.exchange(method, path, body, auth=True)
        if status != 200:
            raise ValueError(f"serving request {path} failed {status}: {answer!r}")
        return answer, elapsed, host_us, size

    def counters(self):
        return self.request("GET", "/v1/debug/counters")[0]

    def call(self, command):
        op, body = params(command)
        if self.route == "mcp":
            self.initialize_mcp()
            body.pop("mode", None)
            path = "/mcp"
            body = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": op, "arguments": body}}
        else:
            path = "/v1/wire/" + op
        before = self.counters()
        answer, elapsed, host_us, size = self.request("POST", path, body)
        if self.route == "mcp":
            if "error" in answer or answer["result"].get("isError"):
                raise ValueError(f"MCP call failed: {answer!r}")
            answer = answer["result"]["structuredContent"]
        if host_us is None:
            raise ValueError("serve subject lacks the counters feature's host-time header")
        after = self.counters()
        return {"answer": answer, "host_ns": int(host_us) * 1000,
                "counters": self.delta(before, after), "report_bytes": after["serve.report_bytes"] - before["serve.report_bytes"],
                "encoded_bytes": size,
                "serving_overhead_ns": elapsed - int(host_us) * 1000}, elapsed

    def initialize_mcp(self):
        # request() may have replaced the socket since the previous call.
        if self.connection is None or self.connection.sock is None:
            self.close_connection()
            self.connect()
        if not self.mcp_initialized:
            self.request("POST", "/mcp", {"jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                           "clientInfo": {"name": "norn-performance", "version": "1"}}})
            self.mcp_initialized = True

    def tool_list_lengths(self):
        self.initialize_mcp()
        lengths = {}
        for revision in ("2025-11-25", "2026-07-28"):
            params = {} if revision == "2025-11-25" else {"_meta": {
                "io.modelcontextprotocol/protocolVersion": revision,
                "io.modelcontextprotocol/clientCapabilities": {}}}
            answer, _, _, size = self.request("POST", "/mcp", {
                "jsonrpc": "2.0", "id": 0, "method": "tools/list", "params": params})
            result = answer.get("result")
            if "error" not in answer and isinstance(result, dict) and isinstance(result.get("tools"), list):
                lengths[revision] = size
        return lengths

    def wait_state(self, state, seconds):
        deadline = time.monotonic() + seconds
        while True:
            status = self.request("POST", "/v1/wire/vault_status", {"vault": ADDRESS})[0]
            if status["status"]["published"]["state"]["state"] == state:
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"serve did not reach {state}: {status}")
            time.sleep(0.02)

    def boundary(self, name, *, fresh=False, since=None):
        before = {} if since is not None else self.counters()
        attempts = self.attempts
        hellos_before = self.hellos
        if fresh:
            self.close_connection()
        started = time.perf_counter_ns() if fresh else None
        # The get opens its connection, and opening runs the verified hello, so
        # a fresh boundary still times hello plus get and counts one hello.
        verb, body = params({"op": "get", "target": "bench/small.md"})
        answer, elapsed, host_us, size = self.request("POST", "/v1/wire/" + verb, body)
        finished = time.perf_counter_ns()
        hellos = self.hellos - hellos_before
        if fresh:
            elapsed = finished - started
        # Count connection attempts before the following instrumentation call.
        connections = self.attempts - attempts
        after = self.counters()
        self.first_use.append({"boundary": name, "elapsed_ns": finished - since if since is not None else elapsed,
                               "host_ns": int(host_us) * 1000, "report_bytes": size,
                               "counters": after if since is not None else self.delta(before, after), "hellos": self.hellos if since is not None else hellos,
                               "connection_attempts": self.attempts if since is not None else connections})
        return {"answer": answer}

    def finish_first_use(self):
        self.boundary("fresh_connection_to_answer", fresh=True)
        self.wait_state("unattached", 150)
        self.boundary("first_get_after_idle_detach")
        return self.first_use

    def close(self):
        self.close_connection()
        stop_process(self.process)
        self.note.unlink(missing_ok=True)
        self.reader.join(timeout=5)
        self.process.stderr.close()
        self.stdout.close()
        self.stderr.close()
