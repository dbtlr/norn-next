"""Public prefix validation keeps real trust, content, cost and declines."""
import base64
import copy
import json
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from test_whole_get import example
from public_mode import public_answer, account, validate_attempt
from revalidation import COMMANDS, GET_WORK, FIND_WORK


def event(index=0, ready=False):
    response, manifest = example()
    trust = {'state':'ready'} if ready else dict(state='warming', phase='healing', healed=1, total_estimate=314)
    response['answer']['reading']['trust'] = trust
    if index:
        manifest = dict(active_paths=[f'bench/{i:02}.md' for i in range(30)])
        snapshot = dict(epoch='store', generation=3, schema_fingerprint=None, sidecar_revision=None)
        key = dict(row='document', order=dict(key=dict(by='path'), direction='ascending'), sort=None, path='bench/24.md')
        token = base64.urlsafe_b64encode(json.dumps(dict(snapshot=snapshot,key=key)).encode()).decode().rstrip('=')
        response['answer']['report'] = dict(page=dict(rows=[dict(path=p) for p in manifest['active_paths'][:25]],next=token,moved=[]),rule_sets=[])
    result = dict(startup_stage=('public-get','public-find')[index], probe=('whole_slug','active_page')[index],
        operator_prefix_sequence=index+1, command=COMMANDS[index], mode='public',
        availability_scope='public Host first-open requests', disposition='served',
        answer=response['answer'], error=None,
        counters=dict(snapshots_opened=1,statements_executed=3,vm_steps=20,full_scan_steps=0),
        builder_work={k:(2 if k.endswith('_statements') else 0) for k in (GET_WORK if index==0 else FIND_WORK)},
        query_started_ns=10,query_finished_ns=30,query_host_ns=20,
        state_after_call=trust, state_observed_ns=31, early_answer=not ready)
    return result, manifest


class PublicReceipts(unittest.TestCase):
    def test_actual_warming_and_ready_answers_use_same_content_oracle(self):
        warming, manifest=event()
        ready,_=event(ready=True)
        self.assertEqual(public_answer(0,warming,manifest),public_answer(0,ready,manifest))
        for trust in ({'state':'untrusted'}, {'state':'warming','phase':'releasing_coverage','healed':0,'total_estimate':None}, {'state':'ready','extra':True}):
            wrong=copy.deepcopy(warming);wrong['answer']['reading']['trust']=trust
            with self.subTest(trust=trust), self.assertRaises(ValueError): public_answer(0,wrong,manifest)
        wrong=copy.deepcopy(warming);wrong['answer']['reading']['epoch']=''
        with self.assertRaises(ValueError): public_answer(0,wrong,manifest)

    def test_no_ready_answer_or_answer_finished_after_ready_can_claim_early(self):
        for ready in (False,True):
            row,_=event(ready=ready)
            row['state_after_call']={'state':'ready'};row['early_answer']=True
            with self.assertRaises(ValueError):validate_attempt(0,row)
        row,_=event();row['query_host_ns']=0
        with self.assertRaises(ValueError):validate_attempt(0,row)

    def test_full_content_and_cursor_are_preserved(self):
        row,manifest=event();row['answer']['report']['document']['body']['text']='wrong'
        with self.assertRaises(ValueError):public_answer(0,row,manifest)
        row,manifest=event(1);public_answer(1,row,manifest)
        for field in ('key','snapshot'):
            wrong=copy.deepcopy(row)
            token=wrong['answer']['report']['page']['next'];cursor=json.loads(base64.urlsafe_b64decode(token+'='*(-len(token)%4)))
            cursor[field]['path' if field=='key' else 'epoch']='wrong'
            wrong['answer']['report']['page']['next']=base64.urlsafe_b64encode(json.dumps(cursor).encode()).decode()
            with self.assertRaises(ValueError):public_answer(1,wrong,manifest)

    def test_refusal_preserves_cost_and_has_no_invented_builder_account(self):
        row,_=event();row.update(disposition='unavailable',answer=None,counters=None,builder_work=None,early_answer=False,
            error=dict(code='host/entry-not-ready',message='this vault is still indexing for the first time',detail=dict(code='host/entry-not-ready',state=dict(state='warming',phase='healing',healed=0,total_estimate=314))))
        self.assertIsNone(public_answer(0,row,{}))
        counts=account([row,event(1)[0]],{})
        self.assertEqual(counts['canonical_host_calls'],483)
        self.assertEqual(counts['public_request_attempt_events'],2)
        self.assertEqual(counts['physical_request_attempt_events'],485)
        self.assertIsNone(counts['total_builder_dispatches'])
        self.assertEqual(counts['fallback_count'],1)
        wrong=copy.deepcopy(row);wrong['error']['code']='host/read-failed'
        with self.assertRaises(ValueError):public_answer(0,wrong,{})
        wrong=copy.deepcopy(row);wrong['counters']={}
        with self.assertRaises(ValueError):validate_attempt(0,wrong)

class PublicReplay(unittest.TestCase):
    def test_replay_keeps_worker_and_parent_clocks_separate_and_checks_observed_ready(self):
        from public_mode import validate_history
        rows=[event()[0],event(1,ready=True)[0]]
        rows[0].update(verified=True,parent_received_ns=100,parent_validated_ns=110,parent_ack_published_ns=120)
        rows[1].update(query_started_ns=40,query_finished_ns=60,state_observed_ns=61,
            verified=True,parent_received_ns=130,parent_validated_ns=140,parent_ack_published_ns=150)
        ready=dict(observed_ready_ns=61,attach_ns=61,startup_complete_ns=70)
        validate_history(rows,ready,160)
        wrong=copy.deepcopy(ready);wrong['observed_ready_ns']=60
        with self.assertRaises(ValueError):validate_history(rows,wrong,160)
        wrong=copy.deepcopy(rows);wrong[1]['parent_ack_published_ns']=170
        with self.assertRaises(ValueError):validate_history(wrong,ready,160)


    def test_ready_receipt_has_total_work_and_no_private_construction_claim(self):
        from public_mode import validate_ready
        rows=[event(0)[0],event(1,ready=True)[0]]
        work=dict(documents_derived=320,documents_upserted=320,documents_deleted=0,
            tombstones_recorded=0,changesets_applied=4,links_redecided=10,
            link_health_keys_resolved=10,link_health_candidates_read=10,
            findings_written=3,findings_discarded=0,changeset_read_steps=20,
            document_opens=320,stats=320,walk_dirents=320,recoveries_run=0,rebuilds_run=0,watcher_polls=1)
        ready=dict(ready=True,operator_prefix_mode='public',timings=None,changeset_size=128,
            store_page_size=128,writer_cache=None,initial_import_cache_kib=None,
            operator_prefix_receipt=dict(mode='public-host-prefix',logical_requests=2,
                dispatch='caller thread through Host::get then Host::find',availability_scope='public Host first-open requests'),
            job_evidence=work)
        result=validate_ready(rows,ready,dict(document_count=314))
        self.assertEqual(result['total_work'],work)
        self.assertIsNone(result['canonical_work'])
        wrong=copy.deepcopy(ready);wrong['operator_prefix_receipt']['construction_eligibility_gets']=[]
        with self.assertRaises(ValueError):validate_ready(rows,wrong,dict(document_count=314))

    def test_replay_requires_public_mode_and_exact_counts_before_slicing(self):
        from public_mode import validate_slice
        with self.assertRaises(ValueError):
            validate_slice(None,dict(mode='candidate-live-revalidated'),[],[],{},[],{},{},lambda x:x)

if __name__=='__main__':unittest.main()
