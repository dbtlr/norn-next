"""Behavior checks for the serial loading comparison controller."""
import sqlite3
from contextlib import closing
import tempfile
import unittest
from pathlib import Path

from loading_matrix import plan, gate_semantics, documents_layout, validate_lab_stage


class MatrixTests(unittest.TestCase):
    def test_mid_target_is_late_in_path_order_outside_initial_batch(self):
        from loading_matrix import mid_request_target
        paths = [f'notes/{number:04}.md' for number in range(300)]
        manifest = {'paths': ['bench/small.md', 'bench/large.md'] + paths}
        target = mid_request_target(manifest)
        self.assertEqual(target, 'notes/0269.md')
        self.assertGreater(sorted(manifest['paths']).index(target), 128)
        with self.assertRaisesRegex(ValueError, 'outside the first'):
            mid_request_target({'paths': paths[:100]})

    def test_early_get_requires_both_explicit_loading_flags(self):
        from loading_matrix import validate_loading_flags
        valid = {'link_resolution_loading': True, 'link_health_loading': True}
        validate_loading_flags(valid)
        for wrong in ({}, {'link_health_loading': True},
                      dict(valid, link_resolution_loading=False)):
            with self.subTest(wrong=wrong), self.assertRaisesRegex(ValueError, 'loading flags'):
                validate_loading_flags(wrong)

    def test_linked_ready_get_retains_archived_health_once_oracle_signature(self):
        import json
        from unittest.mock import patch
        from results import whole_get_answer
        from loading_matrix import validate_link_get
        response = {'answer': json.loads(Path(__file__).with_name('testdata').joinpath('linked_get.json').read_text())}
        manifest = {'large_body': response['answer']['report']['document']['body']['text']}
        # The archived health-once adapter has no first_index keyword.
        with patch('results.whole_get_answer', side_effect=lambda row: whole_get_answer(row)):
            validate_link_get(response, manifest)

    def test_mid_ready_get_keeps_the_archived_health_once_validator_signature(self):
        import json
        from unittest.mock import patch
        from results import whole_get_answer
        from loading_matrix import validate_mid_get
        response = {'answer': json.loads(Path(__file__).with_name('testdata').joinpath('linked_get.json').read_text())}
        document = response['answer']['report']['document']
        manifest = dict(mid_request_path=document['path'], mid_request_body=document['body']['text'])
        with patch('results.whole_get_answer', side_effect=lambda row: whole_get_answer(row)):
            validate_mid_get(response, manifest)

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

    def test_main_controller_rejects_reordered_or_repeated_attempts_before_audit(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        from loading_matrix import measure_cell, PUBLIC_COMMANDS
        import sys
        sys.path.insert(0, str(Path(__file__).parent / 'live_mode'))
        for sequence in ([2, 1, 3], [1, 1, 1], [1, 2, 3]):
            with self.subTest(sequence=sequence), tempfile.TemporaryDirectory() as folder:
                def current(binary, root, **options):
                    for number in sequence:
                        is_get = PUBLIC_COMMANDS[number - 1]['op'] == 'get'
                        state = dict(state='warming', phase='healing', healed=0, total_estimate=314)
                        options['stage_handler'](dict(
                            startup_stage='public-get' if is_get else 'public-find',
                            probe='whole_slug' if is_get else 'active_page',
                            operator_prefix_sequence=number, command=PUBLIC_COMMANDS[number - 1],
                            mode='public', availability_scope='public Host first-open requests',
                            disposition='unavailable', answer=None, counters=None, builder_work=None,
                            error=dict(code='host/entry-not-ready', message='loading',
                                       detail=dict(code='host/entry-not-ready', state=state)),
                            state_after_call=state, early_answer=False, query_started_ns=1,
                            query_finished_ns=2, query_host_ns=1, state_observed_ns=3))
                    return SimpleNamespace(ready=dict(observed_ready_ns=4, job_evidence={},
                        operator_prefix_receipt=dict(logical_requests=3)),
                        process=SimpleNamespace(pid=0), close=lambda: None)
                with patch('transport.Current', side_effect=current), patch('transport.resources', return_value={}), \
                        patch('startup_audit.audit_store', side_effect=RuntimeError('reached audit')) as audit:
                    if sequence == [1, 2, 3]:
                        with self.assertRaisesRegex(RuntimeError, 'reached audit'):
                            measure_cell(Path('worker'), Path(folder), {}, 'main')
                        audit.assert_called_once()
                    else:
                        with self.assertRaisesRegex(ValueError, 'public attempt identity'):
                            measure_cell(Path('worker'), Path(folder), {}, 'main')
                        audit.assert_not_called()

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
            with self.assertRaisesRegex(ValueError, 'product sources differ'):
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
            self.assertEqual(next(row for row in layout['schema'] if row['name'] == 'doc_path'),
                             dict(type='index', name='doc_path', table='documents',
                                  sql='CREATE UNIQUE INDEX doc_path on documents(path)'))
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

    def lab_events(self):
        events = [dict(startup_stage='four-step-' + name, elapsed_ns=ns,
                       parent_received_ns=9000 + ns)
                  for name, ns in [('names', 100), ('request', 120),
                                   ('requested-structure', 160), ('get', 170),
                                   ('content', 300), ('find', 310), ('structure', 400),
                                   ('relations', 500), ('full-text', 600), ('search', 610),
                                   ('complete', 620)]]
        events[1].update(path='late.md', request_ns=120, request_kind='mid-content',
                         content_progress={'completed': 128, 'total': 400})
        events[2].update(path='late.md')
        events[3].update(path='late.md', delivery_ns=170, earliest_available_ns=160,
                         query_started_ns=161, query_finished_ns=170, query_host_ns=9,
                         link_resolution_loading=True, link_health_loading=True)
        steps = ('names', 'content', 'structure', 'relations', 'full-text')
        events[-1].update(reconciled=False, fts_mode='rebuild',
                          steps={key: {'fts_rows_inserted': 400} if key == 'full-text' else {} for key in steps},
                          boundary_ns=dict(zip(steps, (90, 290, 390, 490, 590))),
                          busy_ns=dict(zip(steps, (80, 100, 80, 80, 80))), pull_ahead_ns=40,
                          request_ns={'late.md': 20}, requested_structure_ns={'late.md': 60},
                          delivery_ns={'late.md': 70})
        return events

    def test_lab_measurements_ready_is_step5_and_mid_request_delivery_is_actual(self):
        from loading_matrix import lab_measurements
        events = self.lab_events()
        result = lab_measurements('four-step', events)
        self.assertEqual(result['ready']['source'], 'four-step-full-text')
        self.assertEqual(result['step_boundary_ns']['full-text'], 590)
        self.assertEqual(result['step_busy_ns']['full-text'], 80)
        self.assertEqual(result['step_counters']['full-text']['fts_rows_inserted'], 400)
        mid = result['mid_request']
        self.assertEqual((mid['request_ns'], mid['requested_document_structure_ns'], mid['delivery_ns']),
                         (120, 160, 170))
        self.assertEqual(mid['request_to_delivery_ns'], 50)
        self.assertEqual(result['build_request_to_delivery_ns']['late.md'], 50)
        self.assertEqual(mid['comparison'], 'not comparable (side by side, no ratios)')
        for name in ('four-step-relations', 'four-step-full-text', 'four-step-search'):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'coverage'):
                lab_measurements('four-step', [event for event in events if event['startup_stage'] != name])

    def test_lab_mid_probe_labels_boundary_alignment_and_keeps_bookkeeping_subsets(self):
        from loading_matrix import lab_measurements
        events = self.lab_events()
        bookkeeping = dict(refused_ns={}, request_refusals={},
                           timing_commits={'structure': 3}, timing_ns={'structure': 20})
        events[-1].update(bookkeeping)
        result = lab_measurements('four-step', events)
        self.assertEqual(result['mid_request']['request_alignment'], 'boundary-aligned')
        self.assertIs(result['mid_request']['current_batch_wait_included'], False)
        self.assertIn('best-case', result['mid_request']['latency_scope'])
        self.assertIn('relations includes authority check', result['step_boundary_scope'])
        self.assertIn('full-text includes FTS and finish', result['step_boundary_scope'])
        for key, value in bookkeeping.items():
            self.assertEqual(result[key], value)
        self.assertIn('subset', result['timing_bookkeeping_scope'])
        self.assertIn('after group commit', result['structure_observation_scope'])

    def test_async_mid_request_retains_remaining_chunk_wait_in_its_latency(self):
        from loading_matrix import lab_measurements
        events = self.lab_events()
        mid = next(event for event in events if event.get('request_kind') == 'mid-content')
        mid.update(request_alignment='asynchronous-after-batch', current_batch_wait_included=True,
                   latency_scope='includes remaining host-chunk work and requested derivation')
        result = lab_measurements('four-step', events)
        self.assertTrue(result['mid_request']['current_batch_wait_included'])
        self.assertEqual(result['mid_request']['request_alignment'], 'asynchronous-after-batch')
        self.assertEqual(result['mid_request']['request_to_delivery_ns'], 50)

    def test_lab_mid_request_rejects_early_threshold_and_mixed_build_clocks(self):
        from copy import deepcopy
        from loading_matrix import lab_measurements
        for mutate in (lambda events: events[1]['content_progress'].update(completed=10),
                       lambda events: events[-1]['delivery_ns'].update({'late.md': 10}),
                       lambda events: events[3].update(delivery_ns=119)):
            events = deepcopy(self.lab_events())
            mutate(events)
            with self.assertRaises(ValueError):
                lab_measurements('four-step', events)

    def test_first_answer_labels_use_actual_typed_delivery_and_explicit_loading(self):
        from loading_matrix import describe_probe
        probe = dict(worker_ns=940, path='lab FourStepRead::get', earliest_available_ns=200,
                     delivery_ns=940, query_host_ns=40, query_started_ns=900, query_finished_ns=940,
                     link_resolution_loading=True, link_health_loading=True)
        lab = describe_probe(probe, 'four-step', 'get')
        self.assertEqual(lab['worker_ns'], 940)
        self.assertEqual(lab['delivery_ns'], 940)
        self.assertIn('actual', lab['timing_scope'])
        self.assertIn('link resolution loading', lab['findings_scope'])
        self.assertIn('after build', lab['delivery_scope'])
        for wrong in ({'delivery_ns': 240}, {'earliest_available_ns': 901}, {'query_host_ns': 50}):
            with self.subTest(wrong=wrong), self.assertRaisesRegex(ValueError, 'lab query timing'):
                describe_probe(dict(probe, **wrong), 'four-step', 'get')

    def test_console_names_actual_delivery_and_step5(self):
        from loading_matrix import describe_probe, cell_summary
        result = {'arm': 'four-step', 'ready': {'source': 'four-step-full-text', 'worker_ns': 3_000_000_000},
                  'public_ready': {'worker_ns': 4_000_000_000}, 'store': {'semantic_sha256': 'equal'}}
        for kind in ('get', 'find', 'search'):
            result['first_' + kind] = describe_probe(dict(worker_ns=2_000_000_000,
                earliest_available_ns=1_000_000_000, delivery_ns=2_000_000_000,
                query_started_ns=1_900_000_000, query_finished_ns=2_000_000_000, query_host_ns=100_000_000,
                parent_received_ns=8_000_000_000, path=f'lab FourStepRead::{kind}',
                link_resolution_loading=True, link_health_loading=True), 'four-step', kind)
        summary = cell_summary(314, result)
        self.assertIn('Get[lab FourStepRead::get] actual_delivery 2.000000s', summary)
        self.assertIn('Ready[four-step-full-text]', summary)
        self.assertIn('parent_delivery 8.000000s (not comparable)', summary)
        self.assertNotIn('estimated', summary)

    def test_integrity_gate_requires_one_positive_native_receipt_and_retains_logs(self):
        import json
        import sys
        from loading_matrix import verify_store
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'performance-token').write_text('test-token')
            binary = root / 'worker'
            for rows in ([{'integrity_verified': True}], [{'integrity_verified': False}],
                         [{'integrity_verified': True}, {'integrity_verified': True}]):
                binary.write_text(f'#!{sys.executable}\nimport sys\nassert sys.argv[1] == "verify"\n'
                    + '\n'.join('print(' + repr('NORN_PERFORMANCE ' + json.dumps(row)) + ', flush=True)' for row in rows) + '\n')
                binary.chmod(0o700)
                if rows == [{'integrity_verified': True}]:
                    receipt = verify_store(binary, root, 'post_ready')
                    self.assertTrue(receipt['integrity_verified'])
                    self.assertTrue((root / 'integrity-post_ready.stdout.log').is_file())
                else:
                    with self.assertRaisesRegex(ValueError, 'integrity'):
                        verify_store(binary, root, 'post_ready')

    def test_second_williams_row_can_seed_main_without_an_extra_pilot_cell(self):
        from loading_matrix import selected_cells
        cells = selected_cells([314], ('main', 'four-step', 'four-step-no-body', 'health-once'),
                               repetitions=1, row_start=1)
        self.assertEqual([cell['arm'] for cell in cells],
                         ['main', 'four-step', 'four-step-no-body', 'main', 'health-once'])
        self.assertTrue(cells[0]['discarded_warmup'])
        self.assertTrue(all(not cell['discarded_warmup'] for cell in cells[1:]))
        cells = selected_cells([10014], ('four-step',), 1, skip_warmup=True)
        self.assertEqual(len(cells), 1)
        self.assertEqual(cells[0]['arm'], 'four-step')

    def test_main_cell_uses_preceding_accepted_lab_offset_in_the_same_run(self):
        import json
        from unittest.mock import patch
        from loading_matrix import run_matrix, mid_request_target
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            frozen = root / 'frozen'
            (frozen / 'vault').mkdir(parents=True)
            manifest = dict(document_count=314, matrix_fixture_version=2, tree_sha256='frozen',
                            paths=[f'notes/{number:04}.md' for number in range(314)],
                            large_body='[[small#Details]] [Small](small.md#Details) [[matrix-missing-target]]')
            offsets = []
            def cell(command, **kwargs):
                spec = json.loads(Path(command[-1]).read_text())
                offsets.append((spec['arm'], spec['mid_request_offset_ns']))
                result = dict(arm=spec['arm'], document_count=314, fts_mode='rebuild', fixture_sha256='frozen',
                              mid_request=dict(target=mid_request_target(manifest), request_ns=123))
                (Path(spec['root']) / 'result.json').write_text(json.dumps(result))
            with patch('loading_matrix.checked_build', return_value=({'rustc': 'one', 'executable': 'worker'}, root)), \
                    patch('baseline.prepare', return_value=(frozen, manifest)), \
                    patch('provenance.source_tree_digest', return_value='source'), \
                    patch('loading_matrix.await_idle'), patch('loading_matrix.record_host_load'), patch('loading_matrix.gate_semantics'), \
                    patch('loading_matrix.summarize', return_value={}), \
                    patch('loading_matrix.cell_summary', return_value='accepted'), \
                    patch('subprocess.run', side_effect=cell):
                run_matrix({}, root, root / 'output', [314], ('four-step', 'main'), 1, row_start=1)
            self.assertEqual(offsets, [('main', None), ('four-step', None), ('main', 123)])

    def test_main_offset_seed_comes_from_completed_same_mode_lab_run(self):
        import json
        from loading_matrix import mid_request_seeds
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            result = dict(arm='four-step', fts_mode='batched', document_count=314,
                          fixture_sha256='frozen', mid_request=dict(target='late.md', request_ns=123))
            (root / 'result.json').write_text(json.dumps(result))
            run = dict(completed=True, semantic_equal=True, results=[str(root / 'result.json')])
            (root / 'run.json').write_text(json.dumps(run))
            self.assertEqual(mid_request_seeds(root, 'batched')[314],
                             dict(path='late.md', offset_ns=123, fixture_sha256='frozen'))
            with self.assertRaisesRegex(ValueError, 'FTS mode'):
                mid_request_seeds(root, 'rebuild')
            run['completed'] = False
            (root / 'run.json').write_text(json.dumps(run))
            with self.assertRaisesRegex(ValueError, 'completed'):
                mid_request_seeds(root, 'batched')

    def test_build_digest_excludes_checkout_local_measurement_scratch(self):
        import subprocess
        from provenance import source_tree_digest
        with tempfile.TemporaryDirectory() as folder:
            repo = Path(folder)
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            (repo / 'worker.rs').write_text('source')
            before = source_tree_digest(repo)
            (repo / '.t').mkdir()
            (repo / '.t/measurement.json').write_text('first')
            self.assertEqual(source_tree_digest(repo), before)
            (repo / '.t/measurement.json').write_text('second')
            self.assertEqual(source_tree_digest(repo), before)
            (repo / 'worker.rs').write_text('changed')
            self.assertNotEqual(source_tree_digest(repo), before)

    def test_mid_controls_reach_workers_and_ignore_inherited_values(self):
        import json
        import os
        import sys
        from unittest.mock import patch
        from transport import Current
        for mode in ('rebuild', 'batched'):
            for loading in ('four-step', 'operator-prefix-public'):
                with self.subTest(mode=mode, loading=loading), tempfile.TemporaryDirectory() as folder:
                    root = Path(folder)
                    binary = root / 'worker'
                    binary.write_text(f'#!{sys.executable}\n' + 'import os, sys, json\n'
                        + "assert os.environ['NORN_PERFORMANCE_MID_REQUEST_PATH'] == 'late.md'\n"
                        + (f"assert os.environ['NORN_PERFORMANCE_FTS_MODE'] == '{mode}'\n" if loading == 'four-step' else
                           "assert os.environ['NORN_PERFORMANCE_MID_REQUEST_OFFSET_NS'] == '123'\n")
                        + "print('NORN_PERFORMANCE {\"ready\":true}', flush=True)\n"
                        + 'sys.stdin.readline()\n')
                    binary.chmod(0o700)
                    options = ({'loading_options': dict(transaction_bound=128, requested_paths=[],
                                                       fts_mode=mode, mid_request_path='late.md')}
                               if loading == 'four-step' else
                               {'public_mid_request': dict(path='late.md', offset_ns=123)})
                    with patch.dict(os.environ, NORN_PERFORMANCE_FTS_MODE='foreign',
                                    NORN_PERFORMANCE_MID_REQUEST_PATH='foreign'):
                        worker = Current(binary, root, loading=loading, **options)
                        worker.close()

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

    def test_repeated_warm_writes_have_exact_replay_and_independent_medians(self):
        from loading_matrix import cell_workload, warm_workload, warm_write_medians
        steps = warm_workload(10)
        self.assertEqual(sum(name == 'set_field' for name, _, _ in steps), 10)
        self.assertEqual(sum(name == 'rename_hub' for name, _, _ in steps), 10)
        workload = cell_workload(warm_write_repetitions=10)
        writes = [command for command in workload if command['op'] in ('set', 'new', 'move')]
        self.assertEqual(len(writes), 22)
        hubs = [command for command in writes if command.get('from', '').startswith('bench/hub')]
        self.assertEqual(hubs[0]['from'], 'bench/hub.md')
        self.assertEqual(hubs[-1]['to'], 'bench/hub.md')
        medians = warm_write_medians([dict(name='set_field', host_ns=n, round_trip_ns=n+10)
                                      for n in [1, 5, 2, 4]])
        self.assertEqual(medians['set_field'], dict(samples=4, host_ns=3.0, round_trip_ns=13.0))

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
