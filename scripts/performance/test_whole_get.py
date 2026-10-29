"""A first useful Get includes every requested collection and valid provenance."""
import copy
import json
from pathlib import Path
import unittest
from results import validate_first_get, validate_first_get_content


def example():
    source = '---\nstatus: draft\nperf_bucket: probe\n---\n# Small\nBody\n## Details\nSection\n'
    def span(token):
        offset = source.index(token)
        return dict(byte_offset=offset, line=source[:offset].count('\n') + 1,
                    column=offset - source.rfind('\n', 0, offset))
    document = dict(path='bench/small.md', body=dict(text=source.split('---\n', 2)[2],
        byte_length=len(source.split('---\n', 2)[2].encode())),
        fields={key: dict(raw=value) for key, value in [('status', 'draft'), ('perf_bucket', 'probe')]})
    for key in ['headings', 'blocks', 'tags', 'links', 'findings']:
        document[key] = dict(items=[], total=0)
    document['headings'] = dict(total=2, items=[
        dict(level=1, slug='small', text='Small', span=span('# Small')),
        dict(level=2, slug='details', text='Details', span=span('## Details'))])
    response = dict(answer=dict(report=dict(shape='record', document=document, rule_sets=[]),
        unsatisfied=[], advisories=[], reading=dict(trust={'state': 'ready'}, epoch='store', generation=3)))
    return response, dict(small_body=document['body']['text'])


class WholeGetChecks(unittest.TestCase):
    def test_findings_content_wrapper_is_validated(self):
        response, manifest = example()
        document = response['answer']['report']['document']
        document['findings'] = dict(state='known', value=document['findings'])
        validate_first_get(response, manifest)
        for reason in ['loading', 'unreadable']:
            response, manifest = example()
            response['answer']['report']['document']['findings'] = dict(state='unknown', reason=reason)
            with self.subTest(reason=reason), self.assertRaises(ValueError):
                validate_first_get(response, manifest)

    def test_native_rich_answer_rejects_omitted_truncated_and_wrong_fixture_collections(self):
        response, manifest = example()
        validate_first_get(response, manifest)
        for key in ['fields', 'body', 'headings', 'blocks', 'tags', 'links', 'findings']:
            changed = copy.deepcopy(response)
            del changed['answer']['report']['document'][key]
            with self.subTest(omitted=key), self.assertRaises(ValueError):
                validate_first_get(changed, manifest)
        for key in ['headings', 'blocks', 'tags', 'links', 'findings']:
            changed = copy.deepcopy(response)
            changed['answer']['report']['document'][key]['total'] += 1
            with self.subTest(truncated=key), self.assertRaises(ValueError):
                validate_first_get(changed, manifest)
        changed = copy.deepcopy(response)
        changed['answer']['report']['document']['headings']['items'][0]['slug'] = 'wrong'
        with self.assertRaises(ValueError):
            validate_first_get(changed, manifest)

    def test_authored_rich_fixture_checks_every_collection_and_finding_handles(self):
        response = dict(answer=json.loads(Path(__file__).with_name("testdata").joinpath("rich_get.json").read_text()))
        manifest = dict(small_body=response["answer"]["report"]["document"]["body"]["text"],
            whole_get_requirements=dict(target="small", path="bench/small.md", added_links=5,
                required_tag="coldprobe", missing_document="profilemissingcoldtarget",
                missing_heading="ProfileMissingHeading", missing_block="profilemissingblock"))
        validate_first_get(response, manifest)
        for collection in ["headings", "blocks", "tags", "links", "findings"]:
            changed = copy.deepcopy(response)
            del changed["answer"]["report"]["document"][collection]
            with self.subTest(collection=collection), self.assertRaises(ValueError):
                validate_first_get(changed, manifest)
        for key, value in [("id", True), ("id", 0), ("generation", 999999),
                           ("target", "wrong"), ("kind", "wrong")]:
            changed = copy.deepcopy(response)
            changed["answer"]["report"]["document"]["findings"]["items"][0][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                validate_first_get(changed, manifest)
        changed = copy.deepcopy(response)
        findings = changed["answer"]["report"]["document"]["findings"]["items"]
        findings[1]["id"] = findings[0]["id"]
        with self.assertRaisesRegex(ValueError, "duplicate"):
            validate_first_get(changed, manifest)
        changed = copy.deepcopy(response)
        changed["answer"]["report"]["document"]["links"]["items"][0]["health"] = "broken"
        with self.assertRaises(ValueError):
            validate_first_get(changed, manifest)

    def test_an_early_answer_may_read_its_relations_as_loading(self):
        loading = dict(state='unknown', reason='loading')
        warming = {'state': 'warming', 'phase': 'healing'}
        response, manifest = example()
        response['answer']['reading']['trust'] = warming
        response['answer']['report']['document']['findings'] = loading
        validate_first_get_content(copy.deepcopy(response), manifest, first_index=True)
        with self.assertRaises(ValueError):
            validate_first_get_content(copy.deepcopy(response), manifest)
        ready = copy.deepcopy(response)
        ready['answer']['reading']['trust'] = {'state': 'ready'}
        with self.assertRaises(ValueError):
            validate_first_get_content(ready, manifest, first_index=True)
        changed = copy.deepcopy(response)
        changed['answer']['report']['document']['headings']['items'][0]['slug'] = 'wrong'
        with self.assertRaises(ValueError):
            validate_first_get_content(changed, manifest, first_index=True)

    def test_an_early_rich_answer_withholds_only_link_resolution_and_findings(self):
        def early():
            response = dict(answer=json.loads(Path(__file__).with_name("testdata").joinpath("rich_get.json").read_text()))
            response['answer']['reading']['trust'] = {'state': 'warming', 'phase': 'healing'}
            document = response['answer']['report']['document']
            document['findings'] = dict(state='unknown', reason='loading')
            for link in document['links']['items']:
                link.update(health='loading', targets=None)
            return response
        manifest = dict(small_body=early()["answer"]["report"]["document"]["body"]["text"],
            whole_get_requirements=dict(target="small", path="bench/small.md", added_links=5,
                required_tag="coldprobe", missing_document="profilemissingcoldtarget",
                missing_heading="ProfileMissingHeading", missing_block="profilemissingblock"))
        validate_first_get_content(early(), manifest, first_index=True)
        for key, value in [('health', 'broken'), ('targets', dict(candidates=[], total=0)),
                           ('target', 'wrong'), ('anchor', None)]:
            changed = early()
            changed['answer']['report']['document']['links']['items'][0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_first_get_content(changed, manifest, first_index=True)
        changed = early()
        changed['answer']['report']['document']['tags']['items'] = []
        with self.assertRaises(ValueError):
            validate_first_get_content(changed, manifest, first_index=True)

    def test_native_provenance_is_required_and_handles_cannot_alias(self):
        response, manifest = example()
        for key, value in [('epoch', ''), ('generation', True), ('generation', -1), ('trust', 'healing')]:
            changed = copy.deepcopy(response)
            changed['answer']['reading'][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_first_get(changed, manifest)
        del response['answer']['reading']
        with self.assertRaises(ValueError):
            validate_first_get(response, manifest)


if __name__ == '__main__':
    unittest.main()
