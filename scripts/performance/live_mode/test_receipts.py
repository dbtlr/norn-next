"""Offline live dispositions are independent per logical request."""
import sys
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import copy
import unittest
from revalidation import validate_attempt, resolve_rows, live_answer, logical_available, account, FIND_WORK

class Receipts(unittest.TestCase):
    def decline(self, phase='get'):
        index = 0 if phase == 'get' else 1
        return dict(startup_stage='live-'+phase, probe=('whole_slug','active_page')[index],
                    command=({'op':'get','target':'small','whole':True},{'op':'find'})[index],
                    operator_prefix_sequence=index+1, mode='live', global_ready=False,
                    availability_scope='private live capability availability',
                    disposition='construction-declined', builder_invoked=False, counters=None, counters_before_builder=None,
                    projection_epoch=None, projection_generation=None, typed_snapshot=None,
                    builder_work=None, answer=None, withdrawn_answer=None, reason='ineligible')
    def test_account_counts_builders_even_when_acquisition_executes_sql(self):
        declined=self.decline()
        declined['counters']=dict(snapshots_opened=1,statements_executed=1,vm_steps=0,full_scan_steps=0)
        ready=dict(operator_prefix_receipt=dict(construction_eligibility_gets=[{}]))
        for other,dispatches,not_invoked,fallbacks in (
                (self.served(),484,1,1), (self.decline('find'),483,2,2)):
            with self.subTest(disposition=other['disposition']):
                counts=account([declined,other],ready)
                self.assertEqual(counts,dict(logical_prefix_requests=2,
                    canonical_suffix_requests=481,canonical_host_calls=483,
                    private_request_attempt_events=2,physical_request_attempt_events=485,
                    total_builder_dispatches=dispatches,builder_not_invoked_attempts=not_invoked,
                    fallback_count=fallbacks,construction_eligibility_gets=1))

    def test_phase_is_explicit_even_when_get_declines(self):
        event=self.decline()
        self.assertIsNone(validate_attempt(0,event))
        wrong=copy.deepcopy(event);wrong['startup_stage']='live-find'
        with self.assertRaises(ValueError):validate_attempt(0,wrong)
        canonical=[{'source':'canonical get'}, {'source':'canonical find'}]
        private=[None, {'source':'live find'}]
        self.assertEqual(resolve_rows(private,canonical),[canonical[0],private[1]])
    def served(self):
        event=self.decline('find')
        event.update(disposition='served',reason=None,builder_invoked=True,
            counters=dict(snapshots_opened=1,statements_executed=3,vm_steps=20,full_scan_steps=0),
            counters_before_builder=dict(snapshots_opened=1,statements_executed=1,vm_steps=0,full_scan_steps=0),
            projection_epoch='actual',projection_generation=2,
            typed_snapshot=dict(epoch='actual',generation=2,schema_fingerprint=None,sidecar_revision=None),
            builder_work={k:(2 if k=='find_statements' else 0) for k in FIND_WORK},
            answer=dict(report=dict(page=dict(rows=[dict(path='bench/a.md')],next=None,moved=[]),rule_sets=[]),unsatisfied=[],advisories=[]))
        return event
    def test_missing_or_misbound_provenance_and_counters_refused(self):
        event=self.served()
        validate_attempt(1,event)
        for key,value in [('counters',None),('counters_before_builder',None),('projection_epoch','other'),('projection_generation',None),('typed_snapshot',None),('builder_work',{})]:
            wrong=copy.deepcopy(event);wrong[key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):validate_attempt(1,wrong)
        wrong=copy.deepcopy(event);wrong['counters'].pop('vm_steps')
        with self.assertRaises(ValueError):validate_attempt(1,wrong)
        wrong=copy.deepcopy(event);wrong['counters']['statements_executed']=4
        with self.assertRaises(ValueError):validate_attempt(1,wrong)
    def test_withdrawn_is_executed_but_never_available(self):
        event=self.served()
        manifest=dict(active_paths=['bench/a.md'])
        normal=live_answer(1,event,manifest)
        event.update(disposition='withdrawn',reason='Unavailable',withdrawn_answer=event['answer'],answer=None)
        self.assertEqual(live_answer(1,event,manifest),normal)
        self.assertTrue(event['builder_invoked'])
        self.assertFalse(logical_available(event))
        self.assertEqual(resolve_rows([None,None],[1,2]),[1,2])
        wrong=copy.deepcopy(event);wrong['answer']=wrong['withdrawn_answer']
        with self.assertRaises(ValueError):validate_attempt(1,wrong)
    def test_error_cannot_fall_back_and_full_find_is_not_paths_only(self):
        event=self.decline('find');event.update(disposition='error',reason='Store failure')
        with self.assertRaisesRegex(ValueError,'remains an error'):live_answer(1,event,{})
        event=self.served()
        for bad in ([dict(path='bench/a.md',fields={})],[dict(path='wrong.md')]):
            wrong=copy.deepcopy(event);wrong['answer']['report']['page']['rows']=bad
            with self.assertRaises(ValueError):live_answer(1,wrong,dict(active_paths=['bench/a.md']))

if __name__=='__main__':unittest.main()
