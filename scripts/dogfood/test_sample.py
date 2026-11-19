"""The sampler's record format and the report's arithmetic."""
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

import sample

DAY = sample.DAY_SECONDS
MIB = 1 << 20


def line(at, rss=100 * MIB, fds=20, pid=1, build="b1"):
    return {"at": at, "pid": pid, "build": build, "uptime_s": 0, "rss_bytes": rss, "fds": fds, "threads": 10}


def hourly(days, rss_at=lambda hour: 100 * MIB, fds_at=lambda hour: 20, start=1_000_000, **kw):
    return [line(start + hour * 3600, rss_at(hour), fds_at(hour), **kw) for hour in range(days * 24 + 1)]


class Record(unittest.TestCase):
    def test_a_line_carries_exactly_the_documented_fields_in_order(self):
        figures = {"threads": 7, "fds": 9, "rss_bytes": 123, "uptime_s": 45}
        record = sample.build_record(1795075530.9, 42, "0.0.0+abc", figures)
        self.assertEqual(list(record), ["at", "pid", "build", "uptime_s", "rss_bytes", "fds", "threads"])
        self.assertEqual(record["at"], 1795075530)

    def test_lines_append_and_read_back_skipping_a_torn_one(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "norn-dogfood" / "samples.jsonl"
            sample.append_record(path, line(2))
            sample.append_record(path, line(1))
            with path.open("a") as out:
                out.write('{"at": 3, "pid"\n')
            self.assertEqual([s["at"] for s in sample.load_samples(path)], [1, 2])

    def test_state_file_follows_xdg_state_home_then_home(self):
        self.assertEqual(sample.state_file({"XDG_STATE_HOME": "/state"}), Path("/state/norn-dogfood/samples.jsonl"))
        self.assertEqual(sample.state_file({"XDG_STATE_HOME": "relative"}), Path.home() / ".local/state/norn-dogfood/samples.jsonl")
        self.assertEqual(sample.state_file({}), Path.home() / ".local/state/norn-dogfood/samples.jsonl")

    def test_token_file_is_the_channels_app_directory(self):
        env = {"XDG_CONFIG_HOME": "/config"}
        self.assertEqual(sample.token_file("dev", env), Path("/config/norn-dev/tokens.toml"))
        self.assertEqual(sample.token_file("live", env), Path("/config/norn/tokens.toml"))

    def test_port_comes_from_the_channels_config_then_its_default(self):
        with tempfile.TemporaryDirectory() as directory:
            environ = {"XDG_CONFIG_HOME": directory}
            self.assertEqual(sample.channel_port("dev", environ), 28472)
            (Path(directory) / "norn-dev").mkdir()
            (Path(directory) / "norn-dev" / "config.toml").write_text("[endpoint]\nport = 23917\n")
            self.assertEqual(sample.channel_port("dev", environ), 23917)
            self.assertEqual(sample.channel_port("live", environ), 28471)
            (Path(directory) / "norn-dev" / "config.toml").write_text("[serve]\nidle_after = '2s'\n")
            self.assertEqual(sample.channel_port("dev", environ), 28472)
            (Path(directory) / "norn-dev" / "config.toml").write_text("[endpoint]\nport = true\n")
            self.assertEqual(sample.channel_port("dev", environ), 28472)
            (Path(directory) / "norn-dev" / "config.toml").write_text("[endpoint]\nport = 0\n")
            self.assertEqual(sample.channel_port("dev", environ), 28472)

    def test_secret_is_read_from_hex(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "tokens.toml"
            path.write_text('version = 1\n\n[tokens.dogfood]\nsecret = "0aff"\ncreated = 1\n')
            self.assertEqual(sample.read_secret(path, "dogfood"), b"\x0a\xff")


class Figures(unittest.TestCase):
    def test_proc_status_gives_rss_in_bytes_and_threads(self):
        text = "Name:\tnorn\nVmRSS:\t   2048 kB\nThreads:\t25\n"
        self.assertEqual(sample.parse_proc_status(text), (2048 * 1024, 25))

    def test_elapsed_forms(self):
        self.assertEqual(sample.parse_etime("00:05"), 5)
        self.assertEqual(sample.parse_etime("01:02:03"), 3723)
        self.assertEqual(sample.parse_etime("2-01:00:00"), 2 * 86400 + 3600)

    def test_start_ticks_survive_a_command_name_with_spaces(self):
        stat = "1 (norn serve) S " + " ".join(str(n) for n in range(2, 40))
        self.assertEqual(sample.proc_start_ticks(stat), 20)


    def test_lsof_counts_only_numbered_descriptors(self):
        text = (
            "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n"
            "norn 42 u cwd DIR 1,4 640 2 /\n"
            "norn 42 u txt REG 1,4 100 3 /bin/norn\n"
            "norn 42 u 0r CHR 3,2 0 4 /dev/null\n"
            "norn 42 u 1u CHR 3,2 0 4 /dev/null\n"
            "norn 42 u 2u CHR 3,2 0 4 /dev/null\n"
            "norn 42 u 12u IPv4 0x1 0t0 TCP 127.0.0.1:28472 (LISTEN)\n"
        )
        self.assertEqual(sample.count_lsof_descriptors(text), 4)

    def test_proc_net_writes_ipv4_little_endian_and_the_port_in_hex(self):
        self.assertEqual(sample.proc_net_address("127.0.0.1", 28472), "0100007F:6F38")
        self.assertEqual(sample.proc_net_address("127.0.0.2", 80), "0200007F:0050")


class Listener(unittest.TestCase):
    def fake_proc(self, root, listeners):
        """`listeners` maps a pid to the local address its socket listens on."""
        rows = ["  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"]
        for number, (pid, address) in enumerate(listeners.items()):
            inode = 9000 + number
            rows.append(f"   {number}: {address} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 {inode}")
            fd_dir = root / str(pid) / "fd"
            fd_dir.mkdir(parents=True)
            (fd_dir / "3").symlink_to(f"socket:[{inode}]")
        (root / "net").mkdir()
        (root / "net" / "tcp").write_text("\n".join(rows) + "\n")

    def test_the_listener_on_the_dialed_address_is_found_not_another_on_the_same_port(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fake_proc(root, {
                200: sample.proc_net_address("127.0.0.2", 29852),
                100: sample.proc_net_address("127.0.0.1", 29852),
            })
            self.assertEqual(sample.linux_listener_pid(29852, proc=root, ip="127.0.0.1"), 100)

    def test_no_listener_on_the_dialed_address_is_an_error(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fake_proc(root, {200: sample.proc_net_address("127.0.0.2", 29852)})
            with self.assertRaises(LookupError):
                sample.linux_listener_pid(29852, proc=root, ip="127.0.0.1")


class Report(unittest.TestCase):
    def test_slope_is_the_least_squares_fit_in_bytes_per_day(self):
        samples = hourly(2, rss_at=lambda hour: 100 * MIB + hour * MIB)  # 24 MiB per day
        self.assertAlmostEqual(sample.rss_slope_per_day(samples) / MIB, 24.0, places=6)

    def test_slope_needs_two_distinct_times(self):
        self.assertIsNone(sample.rss_slope_per_day([line(5)]))
        self.assertIsNone(sample.rss_slope_per_day([line(5), line(5)]))

    def test_a_steady_host_crosses_nothing(self):
        result = sample.analyze(hourly(3))
        self.assertEqual(result["day1"]["fds_max"], 20)
        self.assertEqual(result["day1"]["rss_median"], 100 * MIB)
        self.assertEqual(result["latest_day"]["samples"], 24)
        self.assertFalse(any(result["crossed"].values()))

    def test_descriptors_at_the_limit_pass_and_one_over_is_crossed(self):
        at_limit = sample.analyze(hourly(2, fds_at=lambda hour: 28 if hour == 48 else 20))
        self.assertEqual(at_limit["fd_limit"], 28)
        self.assertFalse(at_limit["crossed"]["descriptors"])
        over = sample.analyze(hourly(2, fds_at=lambda hour: 29 if hour == 48 else 20))
        self.assertTrue(over["crossed"]["descriptors"])

    def test_day_one_is_the_runs_first_24_hours(self):
        result = sample.analyze(hourly(2, fds_at=lambda hour: 25 if hour == 23 else 20))
        self.assertEqual(result["day1"]["fds_max"], 25)
        self.assertEqual(result["day1"]["samples"], 24)

    def test_a_slope_of_one_percent_of_day_one_median_per_day_is_crossed(self):
        # About 100 MiB median, so the limit is about 1 MiB/day: 2 MiB/day crosses it, 0.5 does not.
        growing = hourly(3, rss_at=lambda hour: 100 * MIB + hour * MIB // 12)
        result = sample.analyze(growing)
        self.assertTrue(result["crossed"]["rss_slope"])
        shallower = hourly(3, rss_at=lambda hour: 100 * MIB + hour * MIB // 48)
        self.assertFalse(sample.analyze(shallower)["crossed"]["rss_slope"])

    def test_a_run_under_a_day_is_not_judged_for_slope(self):
        result = sample.analyze(hourly(0) + hourly(0, rss_at=lambda hour: 900 * MIB, start=1_000_000 + 3600))
        self.assertFalse(result["slope_judged"])
        self.assertFalse(result["crossed"]["rss_slope"])

    def test_reused_pid_and_build_start_a_new_run_when_uptime_drops(self):
        samples = [dict(line(i), uptime_s=uptime)
                   for i, uptime in enumerate([100, 200, 5, 15])]
        self.assertEqual(sample.current_run(samples), samples[2:])
        self.assertEqual(sample.current_run(samples[:2]), samples[:2])

    def test_the_current_run_starts_at_the_last_restart_or_build_change(self):
        old = [line(1, pid=1), line(2, pid=1)]
        new = [line(3, pid=2), line(4, pid=2)]
        self.assertEqual(sample.current_run(old + new), new)
        rebuilt = [line(3, pid=1, build="b2")]
        self.assertEqual(sample.current_run(old + rebuilt), rebuilt)
        self.assertEqual(sample.current_run([]), [])


class Command(unittest.TestCase):
    def run_report(self, samples):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "samples.jsonl"
            path.write_text("".join(json.dumps(s) + "\n" for s in samples))
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                code = sample.main(["--samples", str(path), "report"])
            return code, out.getvalue(), err.getvalue()

    def test_report_exits_zero_when_nothing_is_crossed(self):
        code, out, _ = self.run_report(hourly(3))
        self.assertEqual(code, 0)
        self.assertIn("No threshold crossed.", out)

    def test_report_exits_nonzero_and_names_the_threshold_when_crossed(self):
        code, out, _ = self.run_report(hourly(3, fds_at=lambda hour: 20 + hour // 4))
        self.assertEqual(code, 1)
        self.assertIn("THRESHOLD CROSSED: descriptors", out)

    def test_report_with_no_samples_exits_2(self):
        code, _, err = self.run_report([])
        self.assertEqual(code, 2)
        self.assertIn("no samples", err)


if __name__ == "__main__":
    unittest.main()
