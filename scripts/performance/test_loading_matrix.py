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
        with tempfile.TemporaryDirectory() as folder:
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

    def test_explicit_lab_controls_reach_worker_without_inheriting_foreign_controls(self):
        import json
        import os
        import sys
        from unittest.mock import patch
        from transport import Current, PREFIX
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            binary = root / 'worker'
            binary.write_text(f'#!{sys.executable}\n' + '''import os, json, sys
assert os.environ['NORN_PERFORMANCE_TRANSACTION_BOUND'] == '128'
assert json.loads(os.environ['NORN_PERFORMANCE_REQUESTED_PATHS']) == ['bench/small.md']
assert os.environ['NORN_PERFORMANCE_LOADING'] == 'four-step-no-body'
assert 'NORN_FOREIGN' not in os.environ
print('NORN_PERFORMANCE ' + json.dumps({'ready': True}), flush=True)
for line in sys.stdin:
    if json.loads(line)['op'] == 'stop': break
''')
            binary.chmod(0o755)
            with patch.dict(os.environ, NORN_FOREIGN='ignored', NORN_PERFORMANCE_TRANSACTION_BOUND='999'):
                worker = Current(binary, root, loading='four-step-no-body',
                    loading_options={'transaction_bound': 128, 'requested_paths': ['bench/small.md']})
                worker.close()
            with self.assertRaisesRegex(ValueError, 'controls'):
                Current(binary, root, loading='four-step',
                    loading_options={'transaction_bound': 129, 'requested_paths': []})

    def test_short_workload_reuses_native_provenance_gate_and_rejects_stale_generations(self):
        import sys
        import copy
        sys.path.insert(0, str(Path(__file__).parent / 'live_mode'))
        from native_validation import validate_provenance
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'vault').mkdir()
            history = [{'op': 'find'}]
            response = {'answer': {'reading': {'trust': {'state': 'ready'}, 'epoch': 'test', 'generation': 4},
                                  'report': {'page': {'next': None}}}, 'counters': {}, 'host_ns': 10}
            audit = {'store_epoch': 'test', 'write_generation': 4}
            result = validate_provenance(root, history, [response], audit, workload=history)
            self.assertEqual(result['responses'], 1)
            wrong = copy.deepcopy(response)
            wrong['answer']['reading']['generation'] = 3
            with self.assertRaisesRegex(ValueError, 'provenance'):
                validate_provenance(root, history, [wrong], audit, workload=history)
            with self.assertRaisesRegex(ValueError, 'workload'):
                validate_provenance(root, history, [response], audit, workload=[{'op': 'search'}])

    def test_workload_contains_all_twelve_operations_and_exactly_four_readbacks(self):
        from loading_matrix import cell_workload
        workload = cell_workload()
        self.assertEqual(len(workload), 19)
        self.assertEqual([c['op'] for c in workload[3:11]], ['search'] * 4 + ['find'] + ['get'] * 3)
        self.assertEqual([c['op'] for c in workload[11:]], ['set', 'get', 'new', 'get', 'move', 'get', 'move', 'get'])

    def test_health_once_runs_archived_exactly_once_gate_with_audited_count(self):
        from loading_matrix import health_once_final_gate
        from unittest.mock import Mock
        validator = Mock()
        health_once_final_gate(validator, ['facts', 'health'], {'ready': True}, {},
                               {'counts': {'links': 73}})
        validator.assert_called_once_with('health-once', ['facts', 'health'], {'ready': True}, {}, stored_links=73)
