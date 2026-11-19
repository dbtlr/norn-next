"""Serving-engine isolation and measurement contracts."""
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from serve import Serve, hello_mac, hello_mac_input, launch_command, other_servers, params, socket_address, subject_config


class ServingEngine(unittest.TestCase):
    def test_a_counter_delta_leaves_out_the_gauge(self):
        before = {"serve.dispatches": 4, "serve.alive_tasks": 3}
        after = {"serve.dispatches": 5, "serve.alive_tasks": 1}
        self.assertEqual(Serve.delta(before, after), {"serve.dispatches": 1})

    def test_quiet_host_includes_resident_real_servers_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid, command in [(1, b"/bin/norn\0serve\0--port\020123\0"),
                                 (2, b"/bin/norn\0get\0"), (3, b"/bin/other\0serve\0"),
                                 (4, b"/isolated/binaries/serve\0serve\0")]:
                (root / str(pid)).mkdir()
                (root / str(pid) / "cmdline").write_bytes(command)
            self.assertEqual(other_servers(root), [1, 4])

    def test_the_subject_starts_auto_launched_with_its_port_in_the_config(self):
        import tomllib
        self.assertEqual(launch_command(Path("/bin/norn")), ["/bin/norn", "serve", "--auto-launched"])
        config = tomllib.loads(subject_config(23456))
        self.assertEqual(config["endpoint"]["port"], 23456)
        # An auto-launched host idles out; the run holds the setting at its maximum.
        self.assertEqual(config["serve"]["auto_exit_after"], "24h")
        self.assertEqual(config["serve"]["idle_after"], "1m")

    def test_the_first_answer_boundary_is_named_for_the_auto_launch(self):
        source = (Path(__file__).parent / "serve.py").read_text()
        self.assertIn('"first_answer_after_auto_launch"', source)
        self.assertNotIn("first_answer_after_start", source)

    def test_native_get_retains_the_whole_target_and_asks_for_every_column(self):
        verb, body = params({"op": "get", "target": "bench/large.md#Details"})
        self.assertEqual(verb, "get")
        self.assertEqual(body["target"], "bench/large.md#Details")
        self.assertEqual(body["columns"], [])

    def test_header_is_the_host_term_and_counter_snapshots_surround_the_call(self):
        subject = Serve.__new__(Serve)
        subject.route = "wire"
        with patch.object(subject, "counters", side_effect=[{"calls": 10, "serve.report_bytes": 0}, {"calls": 11, "serve.report_bytes": 128}]), \
             patch.object(subject, "request", return_value=({"report": {}}, 9000, "5", 128)):
            response, elapsed = subject.call({"op": "get", "target": "x.md"})
        self.assertEqual(elapsed, 9000)
        self.assertEqual(response["host_ns"], 5000)
        self.assertEqual(response["serving_overhead_ns"], 4000)
        self.assertEqual(response["counters"], {"calls": 1, "serve.report_bytes": 128})
        self.assertEqual(response["report_bytes"], 128)

    def test_a_shipped_build_without_the_timing_instrument_is_refused(self):
        subject = Serve.__new__(Serve)
        subject.route = "wire"
        with patch.object(subject, "counters", return_value={}), \
             patch.object(subject, "request", return_value=({}, 9000, None, 2)):
            with self.assertRaisesRegex(ValueError, "lacks the counters feature"):
                subject.call({"op": "get", "target": "x.md"})

    def test_the_serving_identity_requires_a_separate_receipt_and_freezes_it(self):
        import hashlib
        from types import SimpleNamespace
        from record import identity
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "norn"
            binary.write_bytes(b"frozen executable")
            output = root / "run"
            output.mkdir()
            receipt = {"subject": "serve", "features": ["counters"], "executable": str(binary),
                       "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                       "rustc": "release: 1.97.1", "repository_head": "abc", "repository_status": ""}
            args = SimpleNamespace(engine="serve", serve_build=root / "serve.json", output=output)
            with patch("record.verify_build", return_value=receipt) as verify:
                frozen, evidence = identity(args)
            verify.assert_called_once_with(args.serve_build)
            self.assertEqual(frozen.read_bytes(), binary.read_bytes())
            self.assertEqual(evidence["build_sha256"], receipt["sha256"])
            receipt["subject"] = "current"
            with patch("record.verify_build", return_value=receipt):
                with self.assertRaisesRegex(ValueError, "own release counters receipt"):
                    identity(args)

    def test_served_reads_reach_the_current_known_answer_oracle(self):
        from types import SimpleNamespace
        from baseline import verified_call
        command = {"op": "get", "target": "bench/small.md"}
        response = {"answer": {"report": {"document": {"path": "bench/small.md"}}}}
        worker = SimpleNamespace(call=lambda _: (response, 123))
        with patch("baseline.validate_read") as validate:
            observed = verified_call(worker, "current", Path("/fixture"), {}, {}, "get_small", command, 0)
        validate.assert_called_once_with("current", command, response, {})
        self.assertEqual(observed["elapsed_ns"], 123)

    def test_a_fresh_connection_times_hello_and_get_together(self):
        from unittest.mock import Mock
        subject = Serve.__new__(Serve)
        subject.route = "wire"
        subject.connection = Mock()
        subject.attempts = 1
        subject.hellos = 1
        subject.first_use = []
        paths = []
        def request(method, path, body, **options):
            paths.append(path)
            if subject.connection is None:
                # Opening a connection runs its verified hello inside the request.
                subject.connection = Mock()
                subject.attempts += 1
                subject.hellos += 1
            return ({"report": {}}, 9000, "5", 128)
        with patch.object(subject, "request", side_effect=request), \
             patch.object(subject, "counters", side_effect=[{"serve.token_lookups": 10}, {"serve.token_lookups": 13}]), \
             patch("serve.time.perf_counter_ns", side_effect=[1000, 20000]):
            subject.boundary("fresh_connection_to_answer", fresh=True)
        self.assertEqual(paths, ["/v1/wire/get"])
        self.assertEqual(subject.first_use[0]["elapsed_ns"], 19000)
        self.assertEqual(subject.first_use[0]["hellos"], 1)
        self.assertEqual(subject.first_use[0]["connection_attempts"], 1)
        self.assertEqual(subject.first_use[0]["counters"]["serve.token_lookups"], 3)


SECRET = bytes(range(100, 132))
NONCE = bytes(range(32))


def spelled_input():
    """The Rust test's expected bytes, written out literally."""
    return (b"norn-hello-v1\x00" + b"\x00\x00\x00\x20" + bytes(range(32))
            + b"\x00\x00\x00\x03dev" + b"\x00\x00\x00\x0e0.1.0+3f2a9c1e"
            + b"\x00\x00\x00\x08explicit" + b"\x00\x00\x00\x07serving"
            + b"\x00\x00\x00\x0f127.0.0.1:28472" + b"\x00\x00\x00\x0f127.0.0.1:54121")


class HelloMac(unittest.TestCase):
    def test_the_mac_input_is_the_bytes_the_rust_contract_spells(self):
        facts = {"channel": "dev", "build": "0.1.0+3f2a9c1e", "launch": "explicit", "state": "serving",
                 "host": "127.0.0.1:28472", "client": "127.0.0.1:54121"}
        self.assertEqual(hello_mac_input(NONCE, facts), spelled_input())

    def test_the_mac_over_that_input_is_a_pinned_digest(self):
        facts = {"channel": "dev", "build": "0.1.0+3f2a9c1e", "launch": "explicit", "state": "serving",
                 "host": "127.0.0.1:28472", "client": "127.0.0.1:54121"}
        self.assertEqual(hello_mac(SECRET, NONCE, facts),
                         "08f7b94fb62df498e8ed222bc6601b066bc4bf8c84e3b051242c9e98703a8575")

    def test_an_ipv6_end_is_written_in_brackets(self):
        self.assertEqual(socket_address("::1", 50000), "[::1]:50000")
        self.assertEqual(socket_address("127.0.0.1", 7), "127.0.0.1:7")


class FakeHost:
    """A loopback host answering the hello as told and recording bearers."""

    def __init__(self, mode="good", channel="dev"):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import json
        import threading
        self.bearers = []
        self.nonces = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def reply(self, status, body, close=False):
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(data)))
                if close:
                    self.send_header("Connection", "close")
                    self.close_connection = True
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
                if "Authorization" in self.headers:
                    outer.bearers.append(self.headers["Authorization"])
                if self.path != "/v1/hello":
                    return self.reply(200, {"report": {}}, close=mode == "close_after_answer")
                outer.nonces.append(body["nonce"])
                facts = {"channel": channel, "build": "b", "launch": "explicit", "state": "serving",
                         "host": socket_address(*self.server.server_address[:2]),
                         "client": socket_address(*self.client_address[:2])}
                if mode == "empty":
                    return self.reply(200, {})
                if mode == "client":
                    facts["client"] = "127.0.0.1:1"
                if mode == "host":
                    facts["host"] = "127.0.0.1:1"
                if mode == "other_channel":
                    facts["channel"] = "live"
                mac = hello_mac(SECRET, bytes.fromhex(body["nonce"]), facts)
                if mode == "mac":
                    mac = "0" * 64
                if mode == "version":
                    return self.reply(200, {"version": 2, **facts, "mac": mac})
                if mode == "status":
                    return self.reply(503, {"version": 1, **facts, "mac": mac})
                self.reply(200, {"version": 1, **facts, "mac": mac}, close=mode == "close_after_hello")

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def subject(self):
        subject = Serve.__new__(Serve)
        subject.route = "wire"
        subject.port, subject.secret, subject.channel = self.port, SECRET, "dev"
        subject.bearer, subject.attempts, subject.hellos, subject.connection = "bearer-text", 0, 0, None
        return subject

    def stop(self):
        self.server.shutdown()
        self.server.server_close()


class VerifiedHello(unittest.TestCase):
    def host(self, mode):
        host = FakeHost(mode)
        self.addCleanup(host.stop)
        return host

    def test_a_correctly_signed_answer_lets_the_bearer_go(self):
        host = self.host("good")
        subject = host.subject()
        self.addCleanup(subject.close_connection)
        subject.request("POST", "/v1/wire/get", {})
        self.assertEqual(host.bearers, ["Bearer bearer-text"])
        self.assertEqual(subject.hellos, 1)

    def test_every_hello_uses_a_fresh_random_nonce_in_lowercase_hex(self):
        host = self.host("good")
        subject = host.subject()
        self.addCleanup(subject.close_connection)
        for _ in range(2):
            subject.request("POST", "/v1/wire/get", {})
            subject.close_connection()
        self.assertEqual(len(set(host.nonces)), 2)
        for nonce in host.nonces:
            self.assertRegex(nonce, r"^[0-9a-f]{64}$")

    def test_a_reconnect_after_a_close_is_verified_again(self):
        host = self.host("good")
        subject = host.subject()
        self.addCleanup(subject.close_connection)
        subject.request("POST", "/v1/wire/get", {})
        subject.close_connection()
        subject.request("POST", "/v1/wire/get", {})
        self.assertEqual((subject.hellos, subject.attempts, len(host.bearers)), (2, 2, 2))

    def test_a_host_that_closes_after_an_answer_is_verified_again_before_the_next_bearer(self):
        # http.client would reopen a closed socket by itself on the next
        # request; a reopened socket is a new connection whose listener signed
        # no hello, so the engine opens it through a verified hello instead.
        host = self.host("close_after_answer")
        subject = host.subject()
        self.addCleanup(subject.close_connection)
        subject.request("POST", "/v1/wire/get", {})
        subject.request("POST", "/v1/wire/get", {})
        self.assertEqual((subject.hellos, subject.attempts, len(host.nonces), len(host.bearers)),
                         (2, 2, 2, 2))

    def test_a_host_that_closes_after_its_hello_never_sees_the_bearer(self):
        # A connection whose hello answer closed it has no verified socket
        # left to carry a bearer; the engine says so rather than reopening.
        host = self.host("close_after_hello")
        subject = host.subject()
        with self.assertRaisesRegex(ValueError, "hello closed"):
            subject.request("POST", "/v1/wire/get", {})
        self.assertEqual(host.bearers, [])
        self.assertIsNone(subject.connection)

    def test_an_unverified_host_is_refused_and_never_sees_the_bearer(self):
        for mode, check in [("empty", "version"), ("version", "version"), ("status", "status"),
                            ("mac", "mac"), ("client", "client"), ("host", "host"),
                            ("other_channel", "channel")]:
            with self.subTest(mode=mode):
                host = self.host(mode)
                subject = host.subject()
                with self.assertRaisesRegex(ValueError, f"hello {check}"):
                    subject.request("POST", "/v1/wire/get", {})
                self.assertEqual(host.bearers, [])
                self.assertIsNone(subject.connection)
                self.assertEqual(subject.hellos, 0)

class McpCalls(unittest.TestCase):
    def test_mcp_uses_native_params_without_mode_and_preserves_report_byte_counter(self):
        subject = Serve.__new__(Serve)
        subject.route = "mcp"
        report = {"report": {"outcome": "applied"}}
        with patch.object(subject, "initialize_mcp") as initialize, \
             patch.object(subject, "counters", side_effect=[{"serve.report_bytes": 10}, {"serve.report_bytes": 40}]), \
             patch.object(subject, "request", return_value=({"result": {"structuredContent": report}}, 9000, "5", 128)) as request:
            response, elapsed = subject.call({"op": "set", "value": "fresh"})
        initialize.assert_called_once()
        method, path, envelope = request.call_args.args
        self.assertEqual((method, path), ("POST", "/mcp"))
        self.assertEqual(envelope["params"]["name"], "set")
        expected = params({"op": "set", "value": "fresh"})[1]
        expected.pop("mode")
        self.assertEqual(envelope["params"]["arguments"], expected)
        self.assertEqual(response["answer"], report)
        self.assertEqual(response["report_bytes"], 30)
        self.assertEqual(response["encoded_bytes"], 128)
        self.assertEqual(elapsed, 9000)

    def test_tool_list_lengths_records_only_successful_tools_arrays(self):
        subject = Serve.__new__(Serve)
        for invalid in ({"error": {"code": -32600}}, {"result": {}},
                        {"result": {"tools": {}}}):
            with self.subTest(answer=invalid), \
                 patch.object(subject, "initialize_mcp"), \
                 patch.object(subject, "request", side_effect=[
                     (invalid, 0, None, 50),
                     ({"result": {"tools": []}}, 0, None, 100)]):
                self.assertEqual(subject.tool_list_lengths(), {"2026-07-28": 100})

    def test_initialize_is_once_per_connection(self):
        from types import SimpleNamespace
        subject = Serve.__new__(Serve)
        subject.connection = SimpleNamespace(sock=object())
        subject.mcp_initialized = False
        with patch.object(subject, "request") as request:
            subject.initialize_mcp()
            subject.initialize_mcp()
            request.assert_called_once()
            self.assertEqual(request.call_args.args[2]["method"], "initialize")
