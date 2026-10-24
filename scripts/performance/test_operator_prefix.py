"""Private prefix history must preserve full answers and clock provenance."""
import copy
import json
from pathlib import Path
import unittest

from operator_prefix import COMMANDS, validate_answer, validate_history


def rich():
    answer = json.loads(Path(__file__).with_name('testdata').joinpath('rich_get.json').read_text())
    manifest = dict(small_body=answer['report']['document']['body']['text'], active_paths=['a.md'],
        whole_get_requirements=dict(target='small', path='bench/small.md', added_links=5,
            required_tag='coldprobe', missing_document='profilemissingcoldtarget',
            missing_heading='ProfileMissingHeading', missing_block='profilemissingblock'))
    return dict(answer=answer), manifest


class PrefixChecks(unittest.TestCase):
    def test_private_and_native_complete_get_normalize_to_the_same_answer(self):
        native, manifest = rich()
        private = copy.deepcopy(native)
        reading = private['answer'].pop('reading')
        private.update(projection_epoch=reading['epoch'], projection_generation=reading['generation'],
                       simulation='paused', global_ready=False)
        self.assertEqual(validate_answer(0, native, manifest), validate_answer(0, private, manifest, private=True))
        for key in ('headings', 'blocks', 'tags', 'links', 'findings'):
            bad = copy.deepcopy(private)
            del bad['answer']['report']['document'][key]
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_answer(0, bad, manifest, private=True)
        for key in ('projection_epoch', 'projection_generation', 'simulation', 'global_ready'):
            bad = copy.deepcopy(private); del bad[key]
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_answer(0, bad, manifest, private=True)

    def test_find_keeps_full_page_columns_and_cursor_schema_but_checks_provenance(self):
        from base64 import urlsafe_b64encode
        cursor = dict(snapshot=dict(epoch='store', generation=3, schema_fingerprint='schema', sidecar_revision=None),
                      key=dict(kind='document', path='a.md', sort=None))
        response = dict(answer=dict(report=dict(page=dict(rows=[dict(path='a.md')],
            next=urlsafe_b64encode(json.dumps(cursor).encode()).decode().rstrip('='), moved=[]), rule_sets=[]),
            unsatisfied=[], advisories=[], reading=dict(epoch='store', generation=3, trust={'state':'ready'})))
        manifest = dict(active_paths=['a.md'])
        normalized = validate_answer(1, response, manifest)
        self.assertEqual(normalized['report']['page']['next']['snapshot']['schema_fingerprint'], 'schema')
        bad = copy.deepcopy(response); bad['answer']['reading']['generation'] = 4
        with self.assertRaises(ValueError): validate_answer(1, bad, manifest)
        bad = copy.deepcopy(response); bad['answer']['report']['page']['rows'][0]['path'] = 'wrong.md'
        with self.assertRaises(ValueError): validate_answer(1, bad, manifest)

    def test_private_history_rejects_extra_final_missing_wrong_order_and_mixed_clocks(self):
        stages = [dict(startup_stage=stage, probe=probe, command=command,
                       operator_prefix_sequence=i+1, simulation='paused', global_ready=False,
                       availability_scope='private simulated capability availability',
                       stage_boundary_ns=i*10+1, query_started_ns=i*10+2, query_finished_ns=i*10+3,
                       query_host_ns=1, parent_received_ns=i*100+20, parent_validated_ns=i*100+30,
                       parent_ack_published_ns=i*100+40, verified=True)
                  for i, (stage, probe, command) in enumerate([
                      ('canonical-demand','whole_slug',COMMANDS[0]), ('canonical-intake','active_page',COMMANDS[1])])]
        ready = dict(attach_ns=30, startup_complete_ns=31)
        validate_history(stages, ready, parent_ready_received_ns=150)
        for bad in [stages[:1], stages+stages[:1], list(reversed(stages))]:
            with self.assertRaises(ValueError): validate_history(bad, ready, parent_ready_received_ns=150)
        for key, value in [('command',{'op':'count'}), ('global_ready',True), ('operator_prefix_sequence',True),
                           ('parent_validated_ns',151), ('query_finished_ns',31), ('verified',False)]:
            bad = copy.deepcopy(stages); bad[0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_history(bad, ready, parent_ready_received_ns=150)


if __name__ == '__main__': unittest.main()
