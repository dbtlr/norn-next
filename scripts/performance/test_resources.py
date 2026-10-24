"""Kernel status residency fields share the existing resource snapshot read."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from transport import resources


class ResourceChecks(unittest.TestCase):
    def sample(self, status):
        with tempfile.TemporaryDirectory() as directory:
            proc = Path(directory)
            process = proc / '42'
            process.mkdir()
            (process / 'status').write_text(status)
            fields = ['0'] * 20
            fields[0], fields[11], fields[12] = 'S', '125', '75'
            (process / 'stat').write_text('42 (fixture process) ' + ' '.join(fields))
            (process / 'io').write_text('rchar: 11\nwchar: 22\n')
            (process / 'fd').mkdir()
            for name in ('0', '1'):
                (process / 'fd' / name).touch()
            original = Path.read_text
            reads = []

            def read(path, *args, **kwargs):
                reads.append(path.name)
                return original(path, *args, **kwargs)

            with patch('transport.Path', return_value=proc), patch('transport.os.sysconf', return_value=100), \
                    patch.object(Path, 'read_text', autospec=True, side_effect=read):
                actual = resources(42)
            self.assertEqual(reads, ['status', 'stat', 'io'], 'no extra resource file reads')
            return actual

    def test_kernel_residency_axes_and_existing_fields_come_from_one_status_read(self):
        actual = self.sample('Name:\tfixture\nVmRSS:\t1024 kB\nVmHWM:\t2048 kB\n'
                             'RssAnon:\t600 kB\nRssFile:\t400 kB\nRssShmem:\t24 kB\nThreads:\t3\n')
        self.assertEqual(actual, dict(VmRSS_bytes=1048576, VmHWM_bytes=2097152,
            RssAnon_bytes=614400, RssFile_bytes=409600, RssShmem_bytes=24576,
            threads=3, open_fds=2, cpu_seconds=2.0, process_io=dict(rchar=11, wchar=22)))

    def test_absent_kernel_fields_stay_unreported_and_explicit_zero_stays_zero(self):
        actual = self.sample('VmRSS:\t100 kB\nVmHWM:\t200 kB\nRssShmem:\t0 kB\nThreads:\t1\n')
        self.assertNotIn('RssAnon_bytes', actual)
        self.assertNotIn('RssFile_bytes', actual)
        self.assertEqual(actual['RssShmem_bytes'], 0)
        self.assertEqual(actual['VmRSS_bytes'], 102400)


if __name__ == '__main__':
    unittest.main()
