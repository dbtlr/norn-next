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
    def fixture(self, group):
        import copy
        from test_loading_receipts import receipts
        stages, ready, manifest = receipts()
        ready['changeset_size'] = 128
        # The fixed-size test input fits one document changeset.
        ready['job_evidence']['changesets_applied'] = 1
        for stage in stages:
            stage['health_once_work']['changesets_applied'] = 1
        for i, stage in enumerate(stages):
            stage['loading_mode'] = 'health-once-txn'
            r = dict(construction_group=group, construction_commit_attempts=0,
                     construction_commits=0, construction_rollback_attempts=0,
                     construction_rollbacks=0, construction_automatic_rollbacks=0,
                     construction_document_changesets=1, construction_finding_units=0, construction_cleanup_units=0, construction_health_resets=0,
                     construction_health_generation_groups=0, construction_drain_changesets=0, construction_drain_cleanup_units=0, construction_drain_finding_units=0,
                     construction_commits_documents=0, construction_commits_findings=0, construction_commits_cleanup=0, construction_commits_health_reset=0,
                     construction_commits_health_generation=0, construction_commits_drain=0)
            if group == 1:
                r.update(construction_commit_attempts=1, construction_commits=1,
                         construction_commits_documents=1)
            if i:
                r.update(construction_health_resets=1, construction_health_generation_groups=1,
                         construction_commit_attempts=3 if group == 1 else 1,
                         construction_commits=3 if group == 1 else 1,
                         construction_commits_health_reset=1 if group == 1 else 0,
                         construction_commits_health_generation=1)
            stage['health_once_work'].update(r)
        complete = copy.deepcopy(stages[1])
        complete.update(startup_stage='construction-complete', stage_boundary_ns=25)
        complete['construction_work'] = {k: v for k, v in stages[1]['health_once_work'].items()
                                         if k.startswith('construction_')}
        complete.pop('health_once_work')
        stages.append(complete)
        return stages, ready, manifest

    def test_receipts_require_actual_phase_account_and_explicit_matching_group(self):
        import copy
        for group, label in [(1, '1'), (16, '16'), (0, 'unbounded')]:
            stages, ready, manifest = self.fixture(group)
            loading.validate_receipts('health-once-txn', stages, ready, manifest,
                                      stored_links=4, construction=label)
            for key in stages[2]['construction_work']:
                changed = copy.deepcopy(stages)
                changed[2]['construction_work'].pop(key)
                with self.subTest(group=group, key=key), self.assertRaises(ValueError):
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
