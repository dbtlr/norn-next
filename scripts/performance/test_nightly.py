"""The nightly controller leaves no recorder process or serving subject behind."""

import json
import os
import subprocess
import tempfile
import textwrap
import time
import unittest
from pathlib import Path

from nightly import run
from serve import SUBJECT_NOTE, note_subject, running, start_time, stop_group, stop_subjects

HERE = Path(__file__).resolve().parent

# A stand-in recorder: it launches a subject in a session of its own and notes
# it as `Serve` does, starts a helper in its own process group, says which pids
# it made, and then either exits at once or hangs.
RECORDER = textwrap.dedent("""
    import json, subprocess, sys, time
    from pathlib import Path
    output, scripts, mode = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
    sys.path.insert(0, scripts)
    from serve import SUBJECT_NOTE, note_subject
    subject = subprocess.Popen(["sleep", "600"], start_new_session=True)
    # A helper that ignores SIGTERM, as a stuck child can; only SIGKILL ends it.
    helper = subprocess.Popen(["sh", "-c", "trap '' TERM; sleep 600"])
    work = output / "work" / "2000-0"
    work.mkdir(parents=True)
    note_subject(work / SUBJECT_NOTE, subject.pid)
    (output / "pids.json").write_text(json.dumps({"subject": subject.pid, "helper": helper.pid}))
    if mode == "hang":
        time.sleep(600)
    if mode == "exit-on-term":
        import signal
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
        time.sleep(600)
""")


def gone_within(pid, seconds=10):
    """Whether `pid` has exited within `seconds`: an orphan is reaped by init, not here."""
    deadline = time.monotonic() + seconds
    while running(pid):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.02)
    return True


class Cleanup(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        root = Path(self.scratch.name)
        self.script = root / "scripts" / "recorder.py"
        self.script.parent.mkdir()
        self.script.write_text(RECORDER)
        self.output = root / "output"
        self.output.mkdir()

    def made(self):
        pids = json.loads((self.output / "pids.json").read_text())
        for pid in pids.values():
            # Even a failed assertion takes the stand-ins down.
            self.addCleanup(lambda pid=pid: running(pid) and os.kill(pid, 9))
        return pids

    def test_a_timed_out_recorder_takes_its_group_and_its_subject_down(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            # Long enough for the recorder to note its subject; the hang is far longer.
            run(self.script, self.output, HERE, "hang", subjects=self.output, timeout=5)
        pids = self.made()
        self.assertTrue(gone_within(pids["helper"]), "the recorder's process group outlived its timeout")
        self.assertTrue(gone_within(pids["subject"]), "the serving subject outlived its recorder's timeout")
        self.assertEqual(list(self.output.rglob(SUBJECT_NOTE)), [])

    def test_a_recorder_that_exits_on_sigterm_still_loses_a_helper_that_ignores_it(self):
        # The leader exiting on SIGTERM says nothing about the rest of its group.
        with self.assertRaises(subprocess.TimeoutExpired):
            run(self.script, self.output, HERE, "exit-on-term", subjects=self.output, timeout=5)
        pids = self.made()
        self.assertTrue(gone_within(pids["helper"]), "a helper that ignored SIGTERM outlived its recorder")
        self.assertTrue(gone_within(pids["subject"]), "the serving subject outlived its recorder's timeout")

    def test_a_truncated_note_is_skipped_and_later_notes_still_stop_their_subjects(self):
        (self.output / "a").mkdir()
        (self.output / "a" / SUBJECT_NOTE).write_text('{"pid": 12')
        subject = subprocess.Popen(["sleep", "600"], start_new_session=True)
        self.addCleanup(lambda: running(subject.pid) and os.kill(subject.pid, 9))
        (self.output / "b").mkdir()
        note_subject(self.output / "b" / SUBJECT_NOTE, subject.pid)
        self.assertEqual(stop_subjects(self.output, grace=2), [subject.pid])
        subject.wait(timeout=5)
        self.assertEqual(list(self.output.rglob(SUBJECT_NOTE)), [])

    def test_a_note_is_written_whole_or_not_at_all(self):
        note = self.output / SUBJECT_NOTE
        note_subject(note, os.getpid())
        self.assertEqual(json.loads(note.read_text())["pid"], os.getpid())
        self.assertEqual([path.name for path in self.output.iterdir()], [SUBJECT_NOTE])

    def test_a_subject_left_by_a_recorder_that_exited_is_stopped(self):
        run(self.script, self.output, HERE, "exit", subjects=self.output)
        pids = self.made()
        self.assertTrue(gone_within(pids["subject"]), "the serving subject outlived its recorder")
        self.assertEqual(list(self.output.rglob(SUBJECT_NOTE)), [])

    def test_a_note_whose_pid_names_another_process_signals_nothing(self):
        note = self.output / SUBJECT_NOTE
        note_subject(note, os.getpid())
        stale = json.loads(note.read_text())
        stale["start_time"] = start_time(os.getpid()) - 1
        note.write_text(json.dumps(stale))
        self.assertEqual(stop_subjects(self.output), [])
        self.assertFalse(note.exists())

    def test_an_owned_leader_that_exits_on_sigterm_ends_the_wait_at_once(self):
        # Our own child stays a zombie until reaped, and a zombie still holds
        # its group: reaping it while waiting keeps the grace from running out.
        leader = subprocess.Popen(["sleep", "600"], start_new_session=True)
        self.addCleanup(lambda: leader.poll() is None and leader.kill())
        began = time.monotonic()
        stop_group(leader.pid, grace=10, reap=leader.poll)
        self.assertLess(time.monotonic() - began, 5)
        self.assertIsNotNone(leader.poll())

    def test_a_failed_recorder_is_reported_after_its_cleanup(self):
        failing = self.script.parent / "failing.py"
        failing.write_text("import sys\nsys.exit(3)\n")
        with self.assertRaises(subprocess.CalledProcessError) as raised:
            run(failing, subjects=self.output)
        self.assertEqual(raised.exception.returncode, 3)


if __name__ == "__main__":
    unittest.main()
