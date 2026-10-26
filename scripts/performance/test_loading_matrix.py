"""Behavior checks for the serial loading comparison controller."""
import sqlite3
from contextlib import closing
import tempfile
import unittest
from pathlib import Path

from loading_matrix import plan, gate_semantics, documents_layout, validate_lab_stage


class MatrixTests(unittest.TestCase):
    def test_plan_lists_exactly_one_serial_process_per_requested_cell(self):
        cells = plan([2014, 10014])
        self.assertEqual([(c['documents'], c['arm']) for c in cells], [
            (n, arm) for n in (2014, 10014)
            for arm in ('main', 'four-step', 'four-step-no-body', 'health-once')])
        self.assertTrue(all(c['background_documents'] == c['documents'] - 14 for c in cells))
        with self.assertRaises(ValueError):
            plan([2014, 2014])

    def test_semantic_gate_uses_fixture_identity_and_keeps_physical_differences(self):
        references = {}
        gate_semantics(references, {'fixture_sha256': 'one', 'store': {'semantic_sha256': 'a'}})
        gate_semantics(references, {'fixture_sha256': 'two', 'store': {'semantic_sha256': 'b'}})
        with self.assertRaisesRegex(ValueError, 'semantic'):
            gate_semantics(references, {'fixture_sha256': 'one', 'store': {'semantic_sha256': 'b'}})

    def test_dbstat_reports_documents_including_overflow_and_physical_order(self):
        with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parents[2] / 'target') as folder:
            path = Path(folder) / 'store.sqlite3'
            with closing(sqlite3.connect(path)) as db:
                db.execute('pragma page_size=512')
                db.execute('create table documents (path text, body text)')
                db.executemany('insert into documents values (?, ?)', [('a', 'x' * 3000), ('b', 'y' * 3000)])
                db.execute('create table unrelated (value text)')
                db.commit()
            result = documents_layout(path)
            self.assertGreaterEqual(result['overflow_pages'], 10)
            self.assertGreater(result['page_count'], result['overflow_pages'])
            self.assertGreater(result['average_fill'], 0)
            self.assertLessEqual(result['average_fill'], 1)
            self.assertGreaterEqual(result['fragmentation'], 0)
            self.assertLessEqual(result['fragmentation'], 1)

    def test_lab_probe_rejects_wrong_target_and_incomplete_metadata_page(self):
        manifest = {'active_paths': ['a.md', 'b.md']}
        stage = {'startup_stage': 'four-step-find', 'current': True, 'unsatisfied': [], 'advisories': [],
                 'answer': {'page': {'rows': [{'path': 'a.md'}], 'next': None, 'moved': []}, 'rule_sets': []}}
        with self.assertRaisesRegex(ValueError, 'answer size'):
            validate_lab_stage(stage, manifest)
        stage['answer']['page']['rows'].append({'path': 'b.md'})
        validate_lab_stage(stage, manifest)
        with self.assertRaisesRegex(ValueError, 'current'):
            validate_lab_stage({**stage, 'current': False}, manifest)
