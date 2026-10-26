"""Behavior checks for the serial loading comparison controller."""
import sqlite3
from contextlib import closing
import tempfile
import unittest
from pathlib import Path

from loading_matrix import plan, gate_semantics, documents_layout, validate_lab_stage


class MatrixTests(unittest.TestCase):
    def test_linked_early_public_get_keeps_findings_while_warming(self):
        import json
        from loading_matrix import validate_link_get
        response = {'answer': json.loads(Path(__file__).with_name('testdata').joinpath('linked_get.json').read_text())}
        response['answer']['reading']['trust'] = dict(state='warming', phase='healing', healed=1, total_estimate=314)
        manifest = {'large_body': response['answer']['report']['document']['body']['text']}
        validate_link_get(response, manifest, first_index=True)
        response['answer']['report']['document']['findings'] = dict(items=[], total=0)
        with self.assertRaisesRegex(ValueError, 'broken-link finding'):
            validate_link_get(response, manifest, first_index=True)

    def test_main_worker_sources_allow_only_performance_worker_changes(self):
        import subprocess
        from loading_matrix import check_main_source
        with tempfile.TemporaryDirectory() as folder:
            repo = Path(folder)
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            product = repo / 'crates/norn-host/src/lib.rs'
            worker = repo / 'crates/norn-host/tests/performance/public.rs'
            product.parent.mkdir(parents=True)
            worker.parent.mkdir(parents=True)
            product.write_text('product')
            worker.write_text('worker')
            subprocess.run(['git', 'add', '.'], cwd=repo, check=True)
            subprocess.run(['git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                            'commit', '-qm', 'base'], cwd=repo, check=True)
            worker.write_text('ordered worker')
            check_main_source(repo, 'HEAD')
            product.write_text('changed product')
            with self.assertRaisesRegex(ValueError, 'main worker'):
                check_main_source(repo, 'HEAD')
            product.write_text('product')
            (product.parent / 'extra.rs').write_text('untracked product')
            with self.assertRaisesRegex(ValueError, 'main worker'):
                check_main_source(repo, 'HEAD')

    def test_plan_balances_order_and_discards_one_warmup_per_size(self):
        cells = plan([2014, 10014])
        expected = ['main', 'four-step', 'health-once', 'four-step-no-body',
                    'four-step', 'four-step-no-body', 'main', 'health-once',
                    'four-step-no-body', 'health-once', 'four-step', 'main',
                    'health-once', 'main', 'four-step-no-body', 'four-step']
        for size, repetitions in [(2014, 8), (10014, 4)]:
            rows = [c for c in cells if c['documents'] == size]
            self.assertTrue(rows[0]['discarded_warmup'])
            self.assertEqual(sum(c['discarded_warmup'] for c in rows), 1)
            self.assertEqual([c['arm'] for c in rows[1:]], expected * (repetitions // 4))
        self.assertEqual(len(plan([314], 1)), 5)
        with self.assertRaises(ValueError):
            plan([2014, 2014])

    def test_semantic_and_path_id_gates_survive_json_roundtrip(self):
        import copy
        import json
        result = {'fixture_sha256': 'one', 'store': {'semantic_sha256': 'a'},
                  'post_ready_store': {'semantic_sha256': 'initial'},
                  'store_path_ids': {'documents': [('a.md', 1)], 'vault_files': [('a.png', -1)]},
                  'post_ready_store_path_ids': {'documents': [('a.md', 1)], 'vault_files': [('a.png', -1)]}}
        references = {}
        gate_semantics(references, result)
        references = json.loads(json.dumps(references))
        gate_semantics(references, result)
        for phase in ('store', 'post_ready_store'):
            for change in ('semantic', 'documents', 'vault_files'):
                wrong = copy.deepcopy(result)
                if change == 'semantic':
                    wrong[phase]['semantic_sha256'] = 'different'
                else:
                    wrong[phase + '_path_ids'][change][0] = ('a.md', 2)
                with self.subTest(phase=phase, change=change), self.assertRaisesRegex(ValueError, 'path→id'):
                    gate_semantics(references, wrong)
        gate_semantics(references, dict(result, fixture_sha256='two'))

    def test_layout_and_identity_cover_tables_indexes_overflow_and_free_pages(self):
        from loading_method import identity_map, store_layout
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'store.sqlite3'
            with closing(sqlite3.connect(path)) as db:
                db.execute('pragma page_size=512')
                db.execute('create table documents (id integer primary key, path text, body text)')
                db.execute('create unique index doc_path on documents(path)')
                db.execute('create table vault_files (id integer primary key, path text)')
                db.execute("insert into documents values (7,'a.md',?)", ('x' * 3000,))
                db.execute("insert into vault_files values (-3,'a.png')")
                db.commit()
            identity = identity_map(path)
            self.assertEqual(identity, {'documents': [('a.md', 7)], 'vault_files': [('a.png', -3)]})
            layout = store_layout(path)
            self.assertIn('doc_path', layout['objects'])
            self.assertIn('vault_files', layout['objects'])
            self.assertGreater(layout['objects']['documents']['overflow_pages'], 0)
            self.assertEqual(layout['page_count'] * 512, layout['file_size_bytes'])
            self.assertEqual(layout['freelist_count'], 0)

    def test_cpu_admission_uses_own_and_parent_cgroup_quotas(self):
        from loading_method import effective_cpus
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'slice/job').mkdir(parents=True)
            (root / 'cpu.max').write_text('max 100000')
            (root / 'slice/cpu.max').write_text('600000 100000')
            (root / 'slice/job/cpu.max').write_text('800000 100000')
            membership = root / 'membership'
            membership.write_text('0::/slice/job\n')
            self.assertEqual(effective_cpus(12, root, membership), 6)
            self.assertEqual(effective_cpus(2, root, membership), 2)
            (root / 'slice/job/cpu.max').write_text('150000 100000')
            self.assertEqual(effective_cpus(12, root, membership), 1.5)

    def test_host_admission_rejects_each_busy_signal_and_records_bounded_refusal(self):
        from loading_method import busy_reasons, await_idle, BUSY_THRESHOLDS
        from unittest.mock import patch
        sample = dict(loadavg='0 0 0 1/1 1', effective_cpus=2,
                      pressure={kind: {'some': {'avg10': 0}} for kind in ('cpu', 'io', 'memory')})
        self.assertEqual(busy_reasons(sample), [])
        sample['loadavg'] = '2 0 0 1/1 1'
        self.assertEqual(busy_reasons(sample), ['load1_per_cpu'])
        for kind in ('cpu', 'io', 'memory'):
            sample['pressure'][kind]['some']['avg10'] = 100
        with tempfile.TemporaryDirectory() as folder, patch('loading_method.os.sync') as sync, \
                patch('loading_method.host_sample', return_value=sample):
            record = Path(folder) / 'admission.json'
            with self.assertRaisesRegex(RuntimeError, 'host busy'):
                await_idle(record, dict(BUSY_THRESHOLDS, max_wait_seconds=0))
            sync.assert_called_once()
            import json
            audit = json.loads(record.read_text())
            self.assertFalse(audit['accepted'])
            self.assertEqual(len(audit['samples'][0]['busy_reasons']), 4)

    def test_linked_whole_get_rejects_wrong_content_links_findings_and_envelope(self):
        import copy
        import json
        from loading_matrix import validate_link_get
        response = {'answer': json.loads(Path(__file__).with_name('testdata').joinpath('linked_get.json').read_text())}
        manifest = {'large_body': response['answer']['report']['document']['body']['text']}
        validate_link_get(response, manifest)
        mutations = [
            lambda answer: answer.update(unsatisfied=['missing']),
            lambda answer: answer['report']['document']['body'].update(byte_length=1),
            lambda answer: answer['report']['document']['headings'].update(items=[], total=0),
            lambda answer: answer['report']['document']['links']['items'][0].update(target='wrong'),
            lambda answer: answer['report']['document']['links']['items'][0].update(anchor=None),
            lambda answer: answer['report']['document']['links']['items'][0]['span'].update(line=1),
            lambda answer: answer['report']['document']['findings']['items'][0].update(path='wrong.md'),
            lambda answer: answer['report']['document']['findings']['items'][0].update(generation=999999),
            lambda answer: answer['report']['document']['findings']['items'][0].update(message='wrong'),
            lambda answer: answer['report']['document']['findings']['items'][0].update(hint='wrong'),
            lambda answer: answer['report']['document']['findings']['items'][0].update(rule_set='wrong'),
            lambda answer: answer['report']['document']['findings']['items'][0].update(value='wrong'),
        ]
        for number, mutate in enumerate(mutations):
            wrong = copy.deepcopy(response)
            mutate(wrong['answer'])
            with self.subTest(mutation=number), self.assertRaises(ValueError):
                validate_link_get(wrong, manifest)
        response['answer']['report']['document']['findings'] = {'items': [], 'total': 0}
        validate_link_get(response, manifest, health_loading=True)
        wrong = copy.deepcopy(response)
        wrong['answer']['report']['document']['links']['items'][0]['target'] = 'wrong'
        with self.assertRaises(ValueError):
            validate_link_get(wrong, manifest, health_loading=True)

    def test_freeze_preserves_small_and_adds_hash_bound_link_target(self):
        import json
        from unittest.mock import patch
        from loading_matrix import freeze_fixtures
        from fixture import tree_digest
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            fixture = root / 'fixtures/300'
            vault = fixture / 'vault'
            (vault / 'bench').mkdir(parents=True)
            (vault / 'bench/small.md').write_text('unchanged small')
            (vault / 'bench/large.md').write_text('original')
            section = '## Details\nSection\n'
            manifest = dict(tree_sha256=tree_digest(vault), large_body='Large\n' + section,
                            section=section, probe_bodies={'bench/large.md': 'Large\n' + section},
                            paths=['bench/large.md', 'bench/small.md'])
            with patch('loading_matrix.checked_build', return_value=({'executable': '/mock/worker'}, root)), \
                    patch('baseline.prepare', return_value=(fixture, manifest)):
                freeze_fixtures({}, root, [314])
            frozen = json.loads((fixture / 'manifest.json').read_text())
            self.assertEqual((vault / 'bench/small.md').read_text(), 'unchanged small')
            self.assertEqual(frozen['tree_sha256'], tree_digest(vault))
            self.assertNotEqual(frozen['tree_sha256'], frozen['base_fixture_sha256'])
            with patch('loading_matrix.checked_build', return_value=({'executable': '/mock/worker'}, root)), \
                    patch('baseline.prepare', return_value=(fixture, frozen)), \
                    self.assertRaisesRegex(ValueError, 'already enriched'):
                freeze_fixtures({}, root, [314])
            body = (vault / 'bench/large.md').read_text()
            for raw in ('[[small#Details]]', '[Small](small.md#Details)', '[[matrix-missing-target]]'):
                self.assertIn(raw, body)

    def test_generated_2014_base_is_also_hash_bound(self):
        from unittest.mock import patch
        from loading_matrix import freeze_fixtures
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            with patch('loading_matrix.checked_build', return_value=({'executable': '/mock/worker'}, root)), \
                    patch('baseline.prepare', return_value=(root, {'tree_sha256': 'wrong', 'large_body': 'body'})), \
                    self.assertRaisesRegex(ValueError, 'generator hash'):
                freeze_fixtures({}, root, [2014])

    def test_summary_excludes_warmup_and_retains_individual_warm_costs(self):
        import copy
        from loading_method import summarize
        from baseline import READS, WRITES
        counters = {key: 3 for key in ('document_opens', 'documents_derived', 'link_health_keys_resolved',
                    'link_health_candidates_read', 'findings_written', 'links_redecided')}
        probe = dict(worker_ns=10, query_host_ns=2, parent_received_ns=9000)
        result = dict(public_ready=dict(probe, counters=counters), end={'resources': {'VmHWM_bytes': 42, 'cpu_seconds': 1}},
                      warm_operations=[dict(name=name, host_ns=1, round_trip_ns=2) for name in
                                       [row[0] for row in READS] + WRITES], canonical_operations=[],
                      layout={}, ready=probe, first_get=probe, first_find=probe, first_search=probe,
                      first_get_targets={'small': probe}, post_ready_store={}, store={})
        warmup = copy.deepcopy(result)
        warmup['public_ready']['worker_ns'] = 999999
        later = copy.deepcopy(result)
        later['public_ready']['worker_ns'] = 30
        results = [({'documents': 314, 'arm': 'main', 'discarded_warmup': discard}, row)
                   for discard, row in [(True, warmup), (False, result), (False, later)]]
        report = summarize(results)['sizes']['314']['main']
        self.assertEqual(report['comparable']['public_ready_worker_ns'],
                         {'samples': 2, 'median': 20, 'min': 10, 'max': 30})
        for name in [row[0] for row in READS] + WRITES:
            self.assertIn('warm_operations.' + name + '.host_ns', report['comparable'])
            self.assertIn('warm_operations.' + name + '.round_trip_ns', report['comparable'])
        self.assertNotIn('parent_received_ns', report['comparable'])
        self.assertIn('parent_clocks.public_ready', report['not comparable (side by side, no ratios)'])

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

    def test_lab_search_checks_the_common_token_answer_and_lexical_ladder(self):
        from copy import deepcopy
        from fixture import COMMON
        manifest = {'paths': ['a.md', 'b.md']}
        stage = {'startup_stage': 'four-step-search', 'query': COMMON, 'current': True,
                 'unsatisfied': [], 'advisories': [], 'answer': {
                     'page': {'rows': [{'path': 'a.md'}, {'path': 'b.md'}], 'next': None, 'moved': []},
                     'ladder': {'rungs': [{'rung': 'lexical'}], 'repeatable': True}, 'rule_sets': []}}
        validate_lab_stage(stage, manifest)
        wrong = deepcopy(stage)
        wrong['answer']['page']['rows'][0]['path'] = 'foreign.md'
        with self.assertRaisesRegex(ValueError, 'authored workload'):
            validate_lab_stage(wrong, manifest)
        with self.assertRaisesRegex(ValueError, 'current'):
            validate_lab_stage({**stage, 'current': False}, manifest)
        with self.assertRaisesRegex(ValueError, 'query'):
            validate_lab_stage({**stage, 'query': 'different'}, manifest)
        wrong = deepcopy(stage)
        wrong['answer']['ladder']['rungs'] = []
        with self.assertRaisesRegex(ValueError, 'lexical'):
            validate_lab_stage(wrong, manifest)

    def test_lab_measurements_use_report_boundaries_and_separate_pass_and_pull_ahead_work(self):
        from loading_matrix import lab_measurements
        events = [dict(startup_stage='four-step-' + name, elapsed_ns=worker_ns,
                       parent_received_ns=9000 + worker_ns)
                  for name, worker_ns in [('names', 1000), ('requested-structure', 1100),
                                          ('content', 2000), ('find', 2200), ('search', 2400),
                                          ('get', 2600), ('structure', 3000), ('relations', 4000)]]
        events[-1].update(reconciled=False, steps={'names': {'commits': 2}, 'content': {},
                          'structure': {}, 'relations': {}},
                          boundary_ns={'names': 100, 'content': 900, 'structure': 1500, 'relations': 2100},
                          busy_ns={'names': 80, 'content': 500, 'structure': 300, 'relations': 400},
                          pull_ahead_ns=100)
        result = lab_measurements('four-step', events)
        self.assertEqual(result['step_boundary_ns'], {'names': 100, 'content': 900,
                                                      'structure': 1500, 'relations': 2100})
        self.assertEqual(result['step_boundary_intervals_ns'], {'names': 100, 'content': 800,
                                                               'structure': 600, 'relations': 600})
        self.assertEqual(result['step_busy_ns']['content'], 500)
        self.assertEqual(result['pull_ahead_ns'], 100)
        self.assertIn('observer', result['step_busy_scope'])
        self.assertEqual(result['ready']['source'], 'four-step-structure')
        with self.assertRaisesRegex(ValueError, 'coverage'):
            lab_measurements('four-step', [e for e in events if e['startup_stage'] != 'four-step-search'])
        no_body = events[:4] + events[5:7] + [events[4], events[-1]]
        lab_measurements('four-step-no-body', no_body)
        with self.assertRaisesRegex(ValueError, 'coverage'):
            lab_measurements('four-step-no-body', events)

    def test_first_answer_labels_distinguish_builder_time_delivery_and_findings(self):
        from loading_matrix import describe_probe
        probe = {'worker_ns': 940, 'path': 'lab FourStepRead::get', 'earliest_available_ns': 200,
                 'earliest_answer_ns': 240, 'query_host_ns': 40,
                 'query_started_ns': 900, 'query_finished_ns': 940}
        lab = describe_probe(probe, 'four-step', 'get')
        self.assertEqual(lab['worker_ns'], 240)
        self.assertEqual(lab['query_finished_ns'], 940)
        self.assertEqual(lab['timing_scope'], 'estimated earliest availability plus builder query time')
        self.assertIn('not actual finish', lab['worker_clock_scope'])
        self.assertEqual(lab['availability_clock_scope'], 'prerequisite completion observed on worker clock')
        self.assertEqual(lab['findings_scope'], 'answered before link health (health loading)')
        self.assertIn('after build', lab['delivery_scope'])
        for arm in ('main', 'health-once'):
            public = describe_probe({'worker_ns': None, 'path': 'public Host::get after Ready'}, arm, 'get')
            self.assertEqual(public['findings_scope'], 'includes link-health findings')
            self.assertIn('public', public['timing_scope'])

    def test_lab_answer_estimate_rejects_mixed_or_impossible_query_clocks(self):
        from loading_matrix import describe_probe
        probe = {'worker_ns': 940, 'earliest_available_ns': 200, 'earliest_answer_ns': 240,
                 'query_host_ns': 40, 'query_started_ns': 900, 'query_finished_ns': 940}
        for wrong in ({'earliest_answer_ns': 940}, {'earliest_available_ns': 901},
                      {'query_host_ns': 50}, {'query_finished_ns': 899}):
            with self.subTest(wrong=wrong), self.assertRaisesRegex(ValueError, 'lab query timing'):
                describe_probe({**probe, **wrong}, 'four-step', 'get')

    def test_console_names_search_path_and_does_not_present_buffer_delivery_as_answer_time(self):
        from loading_matrix import describe_probe, cell_summary
        result = {'arm': 'four-step', 'ready': {'source': 'four-step-structure',
                  'worker_ns': 3_000_000_000, 'parent_received_ns': 9_000_000_000},
                  'public_ready': {'worker_ns': 4_000_000_000, 'parent_received_ns': 10_000_000_000},
                  'store': {'semantic_sha256': 'equal'}}
        for kind in ('get', 'find', 'search'):
            result['first_' + kind] = describe_probe({'worker_ns': 7_000_000_000,
                'earliest_available_ns': 1_000_000_000, 'earliest_answer_ns': 2_000_000_000,
                'query_started_ns': 6_000_000_000, 'query_finished_ns': 7_000_000_000,
                'query_host_ns': 1_000_000_000,
                'parent_received_ns': 8_000_000_000, 'path': f'lab FourStepRead::{kind}'}, 'four-step', kind)
        summary = cell_summary(314, result)
        self.assertIn('Get[lab FourStepRead::get] estimated_earliest_answer 2.000000s', summary)
        self.assertIn('actual_query_finished 7.000000s', summary)
        self.assertIn('parent_delivery 8.000000s (not comparable)', summary)
        self.assertIn('public_Ready_worker 4.000000s', summary)
        self.assertNotIn('10.000000', summary)
        self.assertIn('Search[lab FourStepRead::search]', summary)
        self.assertIn('answered before link health (health loading)', summary)
        self.assertIn('earliest availability plus builder query time', summary)

    def test_explicit_public_targets_reach_worker_in_order_and_strip_inherited_list(self):
        import json
        import os
        import sys
        from unittest.mock import patch
        from transport import Current
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            binary = root / 'worker'
            binary.write_text(f'#!{sys.executable}\n' + """import os, json, sys
assert json.loads(os.environ['NORN_PERFORMANCE_PUBLIC_GET_TARGETS']) == ['small', 'bench/large.md']
assert os.environ['NORN_PERFORMANCE_LOADING'] == 'operator-prefix-public'
print('NORN_PERFORMANCE ' + json.dumps({'ready': True}), flush=True)
for line in sys.stdin:
    if json.loads(line)['op'] == 'stop': break
""")
            binary.chmod(0o755)
            with patch.dict(os.environ, NORN_PERFORMANCE_PUBLIC_GET_TARGETS='["foreign"]'):
                worker = Current(binary, root, loading='operator-prefix-public',
                                 public_get_targets=['small', 'bench/large.md'])
                worker.close()
            for targets in ([], 'small', ['small', '']):
                with self.subTest(targets=targets), self.assertRaisesRegex(ValueError, 'public Get targets'):
                    Current(binary, root, loading='operator-prefix-public', public_get_targets=targets)

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
        self.assertEqual(len(workload), 20)
        self.assertEqual([c['op'] for c in workload[4:12]], ['search'] * 4 + ['find'] + ['get'] * 3)
        self.assertEqual([c['op'] for c in workload[12:]], ['set', 'get', 'new', 'get', 'move', 'get', 'move', 'get'])

    def test_health_once_runs_archived_exactly_once_gate_with_audited_count(self):
        from loading_matrix import health_once_final_gate
        from unittest.mock import Mock
        validator = Mock()
        health_once_final_gate(validator, ['facts', 'health'], {'ready': True}, {},
                               {'counts': {'links': 73}})
        validator.assert_called_once_with('health-once', ['facts', 'health'], {'ready': True}, {}, stored_links=73)

    def test_prototype_pin_allows_harness_edits_but_rejects_unbuilt_rust(self):
        import subprocess
        from loading_matrix import check_prototype_source
        with tempfile.TemporaryDirectory() as folder:
            repo = Path(folder)
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            (repo / 'crates').mkdir()
            rust = repo / 'crates/worker.rs'
            rust.write_text('compiled')
            (repo / 'driver.py').write_text('compiled driver')
            subprocess.run(['git', 'add', '.'], cwd=repo, check=True)
            subprocess.run(['git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                            'commit', '-qm', 'fixture'], cwd=repo, check=True)
            head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()
            (repo / 'driver.py').write_text('new driver')
            check_prototype_source(repo, head)
            rust.write_text('not built')
            with self.assertRaisesRegex(ValueError, 'prototype Rust'):
                check_prototype_source(repo, head)
            rust.write_text('compiled')
            extra = repo / 'crates/new.rs'
            extra.write_text('untracked and not built')
            with self.assertRaisesRegex(ValueError, 'prototype Rust'):
                check_prototype_source(repo, head)
