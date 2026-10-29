"""Public Host first-open envelopes; observations absent from the API stay unavailable."""
import copy
from revalidation import COMMANDS, COUNTERS, GET_WORK, FIND_WORK, content_answer, integer, counters

MODE = 'candidate-public-revalidated'
LOADING = 'operator-prefix-public'
SCOPE = 'public Host first-open requests'


def trust(value, *, answer=False):
    if value == {'state':'ready'}:
        return 'ready'
    if (isinstance(value,dict) and set(value)=={'state','phase','healed','total_estimate'}
            and value['state']=='warming' and value['phase'] in ('healing','installing_coverage')
            and (not answer or value['phase']=='healing')):
        integer(value['healed'])
        if value['total_estimate'] is not None:integer(value['total_estimate'])
        return 'warming'
    if not answer and value == {'state':'unattached'}:
        return 'unattached'
    raise ValueError('unsupported public trust')


def validate_attempt(index,event):
    """Check the actual VaultAnswer before permitting identity normalization."""
    from native_validation import provenance_epoch, nonnegative_generation, validate_cursor_provenance
    if (type(index) is not int or index not in (0,1)
            or event.get('startup_stage')!=('public-get','public-find')[index]
            or event.get('probe')!=('whole_slug','active_page')[index]
            or event.get('operator_prefix_sequence')!=index+1 or event.get('command')!=COMMANDS[index]
            or event.get('mode')!='public' or event.get('availability_scope')!=SCOPE
            or any(k in event for k in ('global_ready','simulation','counters_before_builder','builder_invoked','construction_eligibility_gets'))):
        raise ValueError('invalid public attempt identity or invented private observation')
    started,finished,cost,observed=(integer(event.get(k)) for k in
        ('query_started_ns','query_finished_ns','query_host_ns','state_observed_ns'))
    if not started < finished <= observed or not 0 < cost == finished-started:
        raise ValueError('public attempt must retain real nonzero elapsed cost')
    after=trust(event.get('state_after_call'))
    answer=event.get('answer')
    if event.get('disposition')=='served':
        if (not isinstance(answer,dict) or set(answer)!={'reading','report','unsatisfied','advisories'}
                or event.get('error') is not None):raise ValueError('missing genuine public VaultAnswer')
        reading=answer['reading']
        if not isinstance(reading,dict) or set(reading)!={'trust','epoch','generation'}:
            raise ValueError('missing public answer reading')
        state=trust(reading['trust'],answer=True)
        epoch=provenance_epoch(reading['epoch']);generation=nonnegative_generation(reading['generation'])
        validate_cursor_provenance(answer['report'],epoch,generation,index)
        counters(event.get('counters'),executed=True)
        work=event.get('builder_work');keys=GET_WORK if index==0 else FIND_WORK
        if not isinstance(work,dict) or set(work)!=keys:raise ValueError('missing public Answered builder work')
        for v in work.values():integer(v)
        statements=work['get_statements' if index==0 else 'find_statements']
        if not 0 < statements <= event['counters']['statements_executed']:
            raise ValueError('public builder work exceeds snapshot work')
        early=state=='warming' and after!='ready'
    else:
        if (event.get('disposition') not in ('unavailable','error') or answer is not None
                or event.get('counters') is not None or event.get('builder_work') is not None):
            raise ValueError('public refusal cannot invent Answered evidence')
        error=event.get('error')
        if (not isinstance(error,dict) or set(error)!={'code','message','detail'}
                or not isinstance(error['message'],str) or not error['message']
                or not isinstance(error['detail'],dict) or error['detail'].get('code')!=error['code']):
            raise ValueError('missing actual public refusal envelope')
        if event['disposition']=='error' or error['code']!='host/entry-not-ready':
            raise ValueError('public operational/storage refusal stops the cell')
        # Only ordinary loading/withdrawal is permitted to await canonical P2.
        detail=error['detail']
        if set(detail)!={'code','state'}:raise ValueError('unsupported loading refusal detail')
        trust(detail['state'])
        early=False
    if type(event.get('early_answer')) is not bool or event['early_answer']!=early:
        raise ValueError('public early label differs from actual trust/Ready observation')
    return answer


def public_answer(index,event,manifest):
    answer=validate_attempt(index,event)
    if answer is None:return None
    response=copy.deepcopy(event)
    response['projection_epoch']=answer['reading']['epoch']
    response['projection_generation']=answer['reading']['generation']
    return content_answer(index,response,manifest,first_index=True)


def logical_available(event):
    return event['disposition']=='served'


def account(stages,ready):
    if len(stages)!=2:raise ValueError('exactly two public attempts required')
    for i,event in enumerate(stages):validate_attempt(i,event)
    return dict(logical_prefix_requests=2,canonical_suffix_requests=481,canonical_host_calls=483,
        public_request_attempt_events=2,private_request_attempt_events=0,physical_request_attempt_events=485,
        served_public_answers=sum(logical_available(s) for s in stages),
        early_public_answers=sum(s['early_answer'] for s in stages),
        total_builder_dispatches=None,builder_not_invoked_attempts=None,
        builder_account_unavailable='refusals expose no builder execution or before-builder counters',
        fallback_count=sum(not logical_available(s) for s in stages),construction_eligibility_gets=None,
        construction_eligibility_unavailable='ordinary public construction has no private eligibility callback')


def validate_ready(stages,ready,manifest):
    if (len(stages)!=2 or ready.get('ready') is not True or ready.get('operator_prefix_mode')!='public'
            or ready.get('timings','missing') is not None or ready.get('changeset_size')!=128
            or ready.get('store_page_size')!=128 or ready.get('writer_cache') is not None
            or ready.get('initial_import_cache_kib') is not None):
        raise ValueError('public prefix controls or two-attempt history differs')
    for i,event in enumerate(stages):validate_attempt(i,event)
    expected=dict(mode='public-host-prefix',logical_requests=2,
        dispatch='caller thread through Host::get then Host::find',availability_scope=SCOPE)
    if ready.get('operator_prefix_receipt')!=expected:
        raise ValueError('public construction cannot claim private receipts')
    total=ready.get('job_evidence')
    if not isinstance(total,dict) or not {'documents_derived','documents_upserted','documents_deleted','tombstones_recorded',
            'changesets_applied','links_redecided','link_health_keys_resolved','link_health_candidates_read',
            'findings_written','findings_discarded','changeset_read_steps'}<=set(total):
        raise ValueError('missing total startup writer work')
    for v in total.values():integer(v)
    if (total['documents_derived']<manifest['document_count']
            or total['documents_upserted']!=total['documents_derived']
            or any(total.get(k)!=0 for k in ('documents_deleted','tombstones_recorded','recoveries_run','rebuilds_run'))):
        raise ValueError('public fresh startup work differs')
    return dict(protocol='public-host-prefix',total_work=total,canonical_work=None,
        canonical_work_unavailable='public job evidence reports total startup work; no canonical/private split',
        construction_eligibility_gets=None,
        construction_eligibility_unavailable='ordinary construction exposes no private eligibility callback',
        unreported_counters=[])


def validate_history(stages,ready,parent_ready):
    previous_worker=previous_parent=0
    observations=[]
    for i,event in enumerate(stages):
        validate_attempt(i,event)
        if event.get('verified') is not True:raise ValueError('unvalidated public attempt')
        started,finished,observed=(integer(event.get(k)) for k in
            ('query_started_ns','query_finished_ns','state_observed_ns'))
        if not previous_worker<=started<finished<=observed<=integer(ready['startup_complete_ns']):
            raise ValueError('public worker clock bounds differ')
        received,validated,ack=(integer(event.get(k)) for k in
            ('parent_received_ns','parent_validated_ns','parent_ack_published_ns'))
        if not previous_parent<=received<=validated<=ack<=parent_ready:
            raise ValueError('public parent clock/ACK bounds differ')
        if event['state_after_call']=={'state':'ready'}:observations.append(observed)
        if observations and event['early_answer']:raise ValueError('early public answer follows observed Ready')
        previous_worker,previous_parent=observed,ack
    observed_ready=integer(ready.get('observed_ready_ns'))
    if (ready.get('attach_ns')!=observed_ready or observed_ready>ready['startup_complete_ns']
            or (observations and observed_ready!=observations[0])
            or (not observations and observed_ready<previous_worker)):
        raise ValueError('observed Ready differs from actual caller observations')
