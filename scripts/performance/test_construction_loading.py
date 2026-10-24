"""Only an explicit mode may select transaction granularity."""
import unittest
import loading


class ConstructionOptions(unittest.TestCase):
    def test_only_supported_groups_are_accepted_for_the_explicit_mode(self):
        for group in ('1', '16', 'unbounded'):
            self.assertEqual(loading.construction_group('health-once-txn', group), group)
        for mode, group in [('health-once', '16'), ('baseline', '1'),
                            ('health-once-txn', None), ('health-once-txn', '0'),
                            ('health-once-txn', '2')]:
            with self.subTest(mode=mode, group=group), self.assertRaises(ValueError):
                loading.construction_group(mode, group)
        self.assertIsNone(loading.construction_group('health-once', None))

    def test_transaction_mode_includes_counter_only_final_drain_boundary(self):
        self.assertEqual(loading.expected_checkpoints('health-once-txn'), {
            ('health-once-facts', 'boundary'), ('health-once-health', 'boundary'),
            ('construction-complete', 'boundary')})

class ConstructionReceipts(unittest.TestCase):
    def fixture(self, group, changesets=1):
        import copy
        from test_loading_receipts import receipts
        stages, ready, manifest = receipts()
        ready['changeset_size'] = 128
        # Three changesets reproduce the native 314-document pilot counts.
        documents = 3 if changesets == 1 else (changesets - 1) * 128 + 58
        manifest['document_count'] = documents
        ready['job_evidence'].update(documents_derived=documents,
            documents_upserted=documents, document_opens=documents)
        ready['job_evidence']['changesets_applied'] = changesets
        for stage in stages:
            stage['health_once_work'].update(changesets_applied=changesets,
                documents_derived=documents, documents_upserted=documents)
        for i, stage in enumerate(stages):
            stage['loading_mode'] = 'health-once-txn'
            r = dict(construction_group=group, construction_commit_attempts=0,
                     construction_commits=0, construction_rollback_attempts=0,
                     construction_rollbacks=0, construction_automatic_rollbacks=0,
                     construction_document_changesets=changesets, construction_finding_units=0, construction_cleanup_units=0, construction_health_resets=0,
                     construction_health_generation_groups=0, construction_drain_changesets=0, construction_drain_cleanup_units=0, construction_drain_finding_units=0,
                     construction_commits_documents=0, construction_commits_findings=0, construction_commits_cleanup=0, construction_commits_health_reset=0,
                     construction_commits_health_generation=0, construction_commits_drain=0)
            if group == 1:
                r.update(construction_commit_attempts=changesets, construction_commits=changesets,
                         construction_commits_documents=changesets)
            if i:
                r.update(construction_health_resets=1, construction_health_generation_groups=changesets,
                         construction_commit_attempts=2 * changesets + 1 if group == 1 else 1,
                         construction_commits=2 * changesets + 1 if group == 1 else 1,
                         construction_commits_health_reset=1 if group == 1 else 0,
                         construction_commits_health_generation=changesets if group == 1 else 1)
            stage['health_once_work'].update(r)
        complete = copy.deepcopy(stages[1])
        complete.update(startup_stage='construction-complete', stage_boundary_ns=25)
        complete['construction_work'] = {k: v for k, v in stages[1]['health_once_work'].items()
                                         if k.startswith('construction_')}
        complete.pop('health_once_work')
        stages.append(complete)
        return stages, ready, manifest

    def test_facts_reject_the_native_premature_g16_and_unbounded_commit_witnesses(self):
        for group, label in [(16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group, changesets=3)
            stages[0]['health_once_work'].update(construction_commit_attempts=1,
                construction_commits=1, construction_commits_health_generation=1)
            with self.subTest(group=group), self.assertRaises(ValueError):
                loading.validate_receipts('health-once-txn', stages, ready, manifest,
                                          stored_links=4, construction=label)

    def test_phase_commits_require_completed_units_including_the_native_g1_witness(self):
        for group, label in [(1, '1'), (16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group, changesets=3)
            for stage in stages:
                work = stage.get('construction_work', stage.get('health_once_work'))
                phase = 'documents' if group == 1 else 'health_generation'
                work['construction_commits_findings'] += work['construction_commits_' + phase]
                work['construction_commits_' + phase] = 0
            with self.subTest(group=group), self.assertRaises(ValueError):
                loading.validate_receipts('health-once-txn', stages, ready, manifest,
                                          stored_links=4, construction=label)

    def test_completed_health_freezes_units_including_both_native_late_generation_witnesses(self):
        import copy
        for group, label in [(16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group, changesets=3)
            for field in ('health_generation_groups', 'finding_units', 'cleanup_units'):
                changed = copy.deepcopy(stages)
                if field == 'health_generation_groups':
                    changed[1]['health_once_work']['construction_' + field] -= 1
                else:
                    changed[2]['construction_work']['construction_' + field] += 1
                with self.subTest(group=group, field=field), self.assertRaises(ValueError):
                    loading.validate_receipts('health-once-txn', changed, ready, manifest,
                                              stored_links=4, construction=label)

    def test_completed_health_freezes_phase_attribution(self):
        stages, ready, manifest = self.fixture(16, changesets=3)
        stages[2]['construction_work'].update(construction_commits_health_reset=1,
                                              construction_commits_health_generation=0)
        with self.assertRaises(ValueError):
            loading.validate_receipts('health-once-txn', stages, ready, manifest, construction='16')

    def test_only_individually_committed_drain_units_may_follow_completed_health(self):
        import copy
        for group, label in [(1, '1'), (16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group, changesets=3)
            work = stages[2]['construction_work']
            work.update(construction_drain_changesets=1, construction_drain_cleanup_units=1,
                        construction_drain_finding_units=1, construction_commits_drain=3)
            work['construction_commits'] += 3
            work['construction_commit_attempts'] += 3
            original = copy.deepcopy((stages, ready))
            loading.validate_receipts('health-once-txn', stages, ready, manifest, construction=label)
            self.assertEqual((stages, ready), original)
            work['construction_commits_drain'] -= 1
            work['construction_commits'] -= 1
            work['construction_commit_attempts'] -= 1
            with self.subTest(group=group), self.assertRaises(ValueError):
                loading.validate_receipts('health-once-txn', stages, ready, manifest, construction=label)

    def bounded_fixture(self, changesets, facts_commits, reset_commits, generation_commits):
        stages, ready, manifest = self.fixture(16, changesets)
        # Worked boundary examples: each document changeset has one generation;
        # the one reset runs between facts and generation judgment.
        for index, stage in enumerate(stages):
            work = stage.get('construction_work', stage.get('health_once_work'))
            commits = facts_commits + (reset_commits + generation_commits if index else 0)
            work.update(construction_commit_attempts=commits, construction_commits=commits,
                        construction_commits_documents=facts_commits,
                        construction_commits_health_reset=reset_commits if index else 0,
                        construction_commits_health_generation=generation_commits if index else 0)
        return stages, ready, manifest

    def test_g16_exact_reset_and_generation_commits_at_full_and_partial_boundaries(self):
        import copy
        # 7+1+7=15 (partial only); 8+1+8=17 (full then partial);
        # 15+1 hits reset; 16 facts fill a group; 17+1+17=35.
        for changesets, facts, reset, generation in [(7, 0, 0, 1), (8, 0, 0, 2),
                (15, 0, 1, 1), (16, 1, 0, 2), (17, 1, 0, 2)]:
            stages, ready, manifest = self.bounded_fixture(changesets, facts, reset, generation)
            loading.validate_receipts('health-once-txn', stages, ready, manifest, construction='16')
            changed = copy.deepcopy(stages)
            for stage in changed[1:]:
                work = stage.get('construction_work', stage.get('health_once_work'))
                work['construction_commits_health_reset'] = 1 - reset
                work['construction_commits_health_generation'] += 1 if reset else -1
            with self.subTest(changesets=changesets), self.assertRaises(ValueError):
                loading.validate_receipts('health-once-txn', changed, ready, manifest, construction='16')

    def test_facts_require_full_groups_even_with_a_plausible_document_phase(self):
        for group, label in [(16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group, changesets=3)
            stages[0]['health_once_work'].update(construction_commits=1,
                construction_commit_attempts=1, construction_commits_documents=1)
            with self.subTest(group=group), self.assertRaisesRegex(ValueError, 'facts commits'):
                loading.validate_receipts('health-once-txn', stages, ready, manifest, construction=label)
        stages, ready, manifest = self.bounded_fixture(16, 1, 0, 2)
        stages[0]['health_once_work'].update(construction_commits=0,
            construction_commit_attempts=0, construction_commits_documents=0)
        with self.assertRaisesRegex(ValueError, 'facts commits'):
            loading.validate_receipts('health-once-txn', stages, ready, manifest, construction='16')

    def test_g16_final_full_group_and_next_partial_count_all_fact_units(self):
        # 7 documents + finding + reset + 7 generations = 16: one full commit.
        # A second finding makes 17: the final partial adds a second commit.
        for findings, commits in [(1, 1), (2, 2)]:
            stages, ready, manifest = self.bounded_fixture(7, 0, 0, commits)
            for stage in stages:
                work = stage.get('construction_work', stage.get('health_once_work'))
                work['construction_finding_units'] = findings
            with self.subTest(total_units=15 + findings):
                loading.validate_receipts('health-once-txn', stages, ready, manifest, construction='16')

    def test_final_health_cannot_add_fact_units_or_fact_phase_commits(self):
        import copy
        stages, ready, manifest = self.bounded_fixture(16, 1, 0, 2)
        for field in ('finding_units', 'cleanup_units', 'commits_documents'):
            changed = copy.deepcopy(stages)
            for stage in changed[1:]:
                work = stage.get('construction_work', stage.get('health_once_work'))
                work['construction_' + field] += 1
                if field == 'commits_documents':
                    work['construction_commits_health_generation'] -= 1
            with self.subTest(field=field), self.assertRaises(ValueError):
                loading.validate_receipts('health-once-txn', changed, ready, manifest, construction='16')

    def test_grouped_facts_do_not_impose_an_unrecorded_phase_order(self):
        for phase in ('documents', 'findings', 'cleanup'):
            stages, ready, manifest = self.bounded_fixture(15, 1, 0, 2)
            # 15 document + 1 finding + 1 cleanup units; any of these phases
            # can own the 16th unit. Reset is 18th; 15 generations end at 33.
            for stage in stages:
                work = stage.get('construction_work', stage.get('health_once_work'))
                work.update(construction_finding_units=1, construction_cleanup_units=1,
                            construction_commits_documents=0)
                work['construction_commits_' + phase] = 1
            with self.subTest(phase=phase):
                loading.validate_receipts('health-once-txn', stages, ready, manifest, construction='16')

    def test_receipts_require_actual_phase_account_and_explicit_matching_group(self):
        import copy
        for group, label in [(1, '1'), (16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group)
            loading.validate_receipts('health-once-txn', stages, ready, manifest,
                                      stored_links=4, construction=label)
            for index in range(3):
                account = 'construction_work' if index == 2 else 'health_once_work'
                for key in stages[2]['construction_work']:
                    for bad in (None, True, -1):
                        changed = copy.deepcopy(stages)
                        if bad is None:
                            changed[index][account].pop(key)
                        else:
                            changed[index][account][key] = bad
                        with self.subTest(group=group, boundary=index, key=key, bad=bad), self.assertRaises(ValueError):
                            loading.validate_receipts('health-once-txn', changed, ready, manifest, construction=label)
            for key, bad in [('construction_group', 8), ('construction_commits', 0),
                             ('construction_commit_attempts', 0), ('construction_rollbacks', 1),
                             ('construction_health_generation_groups', 0),
                             ('construction_commits_drain', 1)]:
                changed = copy.deepcopy(stages)
                changed[1]['health_once_work'][key] = bad
                with self.subTest(group=group, key=key), self.assertRaises(ValueError):
                    loading.validate_receipts('health-once-txn', changed, ready, manifest, construction=label)

    def test_warm_runner_passes_explicit_group_and_validates_before_queries(self):
        import io
        import tempfile
        from pathlib import Path
        from types import SimpleNamespace
        from unittest.mock import patch
        from baseline import run_worker
        from fixture import tree_digest
        stages, ready, manifest = self.fixture(16)
        stages[2]['construction_work'].pop('construction_commits')
        calls = []
        worker = SimpleNamespace(stages=stages, ready=ready,
                                 call=lambda command: calls.append(command), close=lambda: None)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / 'fixture'
            (fixture / 'vault').mkdir(parents=True)
            manifest.update(tree_sha256=tree_digest(fixture / 'vault'), probe_bodies={})
            output = root / 'out'
            args = SimpleNamespace(output=output, current=Path('worker'), loading='health-once-txn',
                                   construction_group='16')
            with patch('baseline.Current', return_value=worker) as current:
                with self.assertRaisesRegex(ValueError, 'construction_commits'):
                    run_worker(args, 'current', 0, 0, fixture, manifest, io.StringIO())
                self.assertEqual(current.call_args.kwargs['construction_group'], '16')
            self.assertEqual(calls, [])

    def test_finding_only_and_cleanup_units_are_separate_from_document_changesets(self):
        for group, label in [(1, '1'), (16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group)
            for index, stage in enumerate(stages):
                work = stage['construction_work' if index == 2 else 'health_once_work']
                work.update(construction_finding_units=1, construction_cleanup_units=1)
                if group == 1:
                    work['construction_commit_attempts'] += 2
                    work['construction_commits'] += 2
                    work.update(construction_commits_findings=1, construction_commits_cleanup=1)
            loading.validate_receipts('health-once-txn', stages, ready, manifest, construction=label)


class ConstructionColdRounds(unittest.TestCase):
    def result(self, mode, group=None):
        return dict(mode=mode, construction_group=group,
                    store=dict(schema_version=3, source_sha256='source',
                               semantic_sha256='semantics', document_construction_sha256='identity'),
                    first_get=dict(normalized_answer={'body': 'complete'}),
                    ready=dict(startup_complete_ns=1,
                               job_evidence={key: 10 for key in set(loading.ORDINARY_WORK + loading.WRITER_WORK)}),
                    stages=[dict(construction_work={
                        'construction_drain_changesets': 0,
                        'construction_drain_cleanup_units': 0,
                        'construction_drain_finding_units': 0})],
                    checkpoint_boundaries_ns={})

    def run_controller(self, modes, mutate=lambda results: None):
        import json
        import sys
        import tempfile
        from pathlib import Path
        from unittest.mock import patch
        from fixture import tree_digest
        from provenance import sha256
        results = [self.result(mode, group) for mode in modes
                   for group in (['1', '16', 'unbounded'] if mode == 'health-once-txn' else [None])]
        # Ordinary eager health does different work, and must not be compared
        # with the deferred-health writer account.
        for result in results:
            if result['mode'] == 'baseline':
                result['ready']['job_evidence']['links_redecided'] = 99
        mutate(results)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / 'fixtures' / '300'
            (fixture / 'vault').mkdir(parents=True)
            (fixture / 'manifest.json').write_text(json.dumps(dict(
                tree_sha256=tree_digest(fixture / 'vault'), document_count=314)))
            binary = root / 'binary'
            binary.write_bytes(b'debug-test-double')
            argv = ['loading.py', '--build', str(root / 'build.json'),
                    '--fixtures', str(root / 'fixtures'), '--output', str(root / 'out'),
                    '--sizes', '300', '--modes', *modes, '--repetitions', '1', '--no-profile']
            if 'health-once-txn' in modes:
                argv += ['--construction-groups', '1', '16', 'unbounded']
            with patch.object(sys, 'argv', argv), patch('loading.verify_build', return_value=dict(
                    features=['induced-failure'], executable=str(binary), sha256=sha256(binary))), \
                 patch('loading.source_tree_digest', return_value='pinned'), \
                 patch('loading.run_one', side_effect=results):
                loading.main()
            self.assertTrue(json.loads((root / 'out' / 'run.json').read_text())['completed'])

    def test_controller_requires_both_independent_references_for_transaction_qualification(self):
        for modes in [['health-once-txn'], ['baseline', 'health-once-txn'],
                      ['health-once', 'health-once-txn']]:
            with self.subTest(modes=modes), self.assertRaises(SystemExit):
                self.run_controller(modes)

    def test_controller_accepts_references_before_or_after_transaction_block_and_plain_baseline(self):
        for modes in [['baseline'], ['baseline', 'layered'], ['baseline', 'health-once'],
                      ['baseline', 'health-once', 'health-once-txn'],
                      ['health-once-txn', 'baseline', 'health-once']]:
            with self.subTest(modes=modes):
                self.run_controller(modes)

    def test_controller_rejects_shared_group_deviations_from_independent_references(self):
        import copy
        changes = [('schema_version', 4), ('source_sha256', 'other-source'),
                   ('semantic_sha256', 'other-semantics'),
                   ('document_construction_sha256', 'other-identity'),
                   ('whole_get', {'body': 'truncated'})]
        changes += [(key, 11) for key in set(loading.ORDINARY_WORK + loading.WRITER_WORK)]
        for modes in [['baseline', 'health-once', 'health-once-txn'],
                      ['health-once-txn', 'baseline', 'health-once']]:
            for key, value in changes:
                def mutate(results):
                    for result in results:
                        if result['mode'] != 'health-once-txn':
                            continue
                        if key == 'whole_get':
                            result['first_get']['normalized_answer'] = copy.deepcopy(value)
                        elif key in result['store']:
                            result['store'][key] = value
                        else:
                            result['ready']['job_evidence'][key] = value
                with self.subTest(modes=modes, key=key), self.assertRaises(ValueError):
                    self.run_controller(modes, mutate)

    def test_controller_rejects_nonempty_captured_drain_for_any_group(self):
        for group in ['1', '16', 'unbounded']:
            for key in ['construction_drain_changesets', 'construction_drain_cleanup_units',
                        'construction_drain_finding_units']:
                def mutate(results):
                    result = next(r for r in results if r['construction_group'] == group)
                    result['stages'][-1]['construction_work'][key] = 1
                with self.subTest(group=group, key=key), self.assertRaises(ValueError):
                    self.run_controller(['baseline', 'health-once', 'health-once-txn'], mutate)
