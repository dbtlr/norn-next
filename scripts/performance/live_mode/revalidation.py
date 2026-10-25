"""Shared complete P2 and canonical suffix oracles for public measurement."""
import copy
import json
import time

SLICE = 'canonical-Get-Find-revalidation-v1'
COMMANDS = ({'op':'get','target':'small','whole':True}, {'op':'find'})
COUNTERS = {'snapshots_opened','statements_executed','vm_steps','full_scan_steps'}
GET_WORK = {'get_statements','get_full_scan_steps','get_sorts','get_vm_steps','get_anchor_headings','get_link_candidates_read'}
FIND_WORK = {'find_statements','find_keys_read','find_page_full_scan_steps','find_page_sorts','find_page_vm_steps',
             'find_documents_hydrated','find_tag_rows','find_heading_rows','find_block_rows','find_link_rows',
             'find_link_candidates_read','find_finding_rows'}


def integer(value):
    if type(value) is not int or value < 0:
        raise ValueError('invalid nonnegative receipt integer')
    return value


def root_identity(root):
    vault=(root/'vault').stat()
    return f'{vault.st_dev:016x}{vault.st_ino:016x}'


def resolve_rows(private, canonical):
    """One canonical pair supplies every unavailable logical answer, once."""
    if len(private)!=2 or len(canonical)!=2:
        raise ValueError('exactly two logical answers required')
    return [p if p is not None else c for p,c in zip(private,canonical)]


def counters(value, *, executed):
    if not isinstance(value,dict) or set(value)!=COUNTERS:
        raise ValueError('missing actual snapshot counters')
    for key,v in value.items():
        integer(v)
        if key!='full_scan_steps' and executed and v==0:
            raise ValueError('executed builder requires actual SQL work')
    if value['snapshots_opened']!=1:
        raise ValueError('one actual snapshot required')


def content_answer(index,response,manifest,*,first_index=False):
    """Shared full content oracle; envelope provenance is validated by the caller."""
    answer=response['answer']
    if index==0:
        from results import validate_first_get_content
        normalized=validate_first_get_content(response,manifest,first_index=first_index)
    else:
        from results import validate_read
        from results import metadata_answer
        validate_read('current',COMMANDS[1],response,manifest)
        normalized=metadata_answer(response)
        normalized.pop('reading',None)
        fold=str.maketrans('ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz')
        ordered=sorted(manifest['active_paths'],key=lambda p:(p.translate(fold),p))
        report=answer['report'];page=report.get('page',{})
        if (set(report)!={'page','rule_sets'} or report['rule_sets']!=[]
                or set(page)!={'rows','next','moved'} or page['moved']!=[]
                or page['rows']!=[dict(path=p) for p in ordered[:25]]
                or (page['next'] is not None)!=(len(ordered)>25)
                or answer['unsatisfied']!=[] or answer['advisories']!=[]):
            raise ValueError('full Find differs from authored complete page oracle')
        if page['next'] is not None:
            cursor=normalized['report']['page']['next']
            key=dict(row='document',order=dict(key=dict(by='path'),direction='ascending'),sort=None,path=ordered[24])
            if (set(cursor)!={'snapshot','key'} or cursor['key']!=key
                    or cursor['snapshot']!=dict(schema_fingerprint=None,sidecar_revision=None)):
                raise ValueError('Find cursor position/order or provenance differs')
    return normalized


def canonical_answer(index, response, manifest, *, expected=None):
    from native_validation import provenance_epoch, nonnegative_generation, validate_cursor_provenance
    from capped_compare import validated_prefix_answer
    if not isinstance(response, dict) or set(response) != {'answer', 'counters', 'host_ns'}:
        raise ValueError('canonical revalidation envelope differs')
    integer(response['host_ns'])
    counters = response['counters']
    keys = {'snapshots_opened', 'statements_executed', 'vm_steps', 'full_scan_steps'}
    if not isinstance(counters, dict) or set(counters) != keys:
        raise ValueError('canonical revalidation snapshot counters differ')
    for key, value in counters.items():
        integer(value)
        if key != 'full_scan_steps' and value == 0:
            raise ValueError('canonical revalidation needs actual snapshot SQL work')
    answer = response['answer']
    reading = answer.get('reading', {})
    if set(reading) != {'trust', 'epoch', 'generation'} or reading.get('trust') != {'state':'ready'}:
        raise ValueError('canonical revalidation requires exact Ready reading')
    observed = (provenance_epoch(reading.get('epoch')), nonnegative_generation(reading.get('generation')))
    if expected is not None and observed != expected:
        raise ValueError('canonical revalidation epoch/initial generation differs')
    validate_cursor_provenance(answer.get('report', {}), *observed, index)
    # The inherited full oracles remove only declared epoch/handle differences.
    return validated_prefix_answer(index, response, manifest), observed


def validate_resources(snapshot):
    """Require the existing Linux endpoint collector's observations, never zeros for absence."""
    import math
    required = {'cpu_seconds','process_io','open_fds','threads','VmRSS_bytes','VmHWM_bytes',
                'RssAnon_bytes','RssFile_bytes','RssShmem_bytes'}
    if not isinstance(snapshot,dict) or not required <= set(snapshot):
        raise ValueError('missing canonical revalidation resource endpoint')
    for key in required - {'cpu_seconds','process_io'}:
        integer(snapshot[key])
    cpu = snapshot['cpu_seconds']
    if type(cpu) not in (int,float) or not math.isfinite(cpu) or cpu < 0:
        raise ValueError('invalid process CPU resource endpoint')
    io = snapshot['process_io']
    if (not isinstance(io,dict) or not {'rchar','wchar','syscr','syscw','read_bytes','write_bytes','cancelled_write_bytes'} <= set(io)
            or any(type(v) is not int for v in io.values())):
        raise ValueError('missing process I/O resource endpoint')


PARENT_KEYS={'parent_received_ns','parent_validated_ns','verified','parent_ack_published_ns',
             'resources_after_validation','resources_after_handler','private_storage_at_validation',
             'construction_storage_high_water','construction_storage_high_water_unavailable'}


def native_stage(row):
    return {k:v for k,v in row.items() if k not in PARENT_KEYS}


def canonical_prefix(worker,root,manifest,parent_ready):
    from baseline import append, write_json
    from transport import Current, resources
    elapsed=lambda:time.perf_counter_ns()-worker.prefix_began
    prefix=dict(slice=SLICE,commands=list(COMMANDS),rows=[],parent_start_ns=elapsed(),resources_before=resources(worker.process.pid))
    expected=None
    for i,command in enumerate(COMMANDS):
        requested=elapsed()
        receipt=dict(slice=SLICE,logical_request=i+1,attempt='canonical-prefix',command=command,
            native_stdout_index=len(worker.stages)+1+i,cell_token=(root/'performance-token').read_text(),root_identity=root_identity(root))
        write_json(root/f'canonical-command-{i+1}.json',receipt)
        append(worker.physical_stream,dict(kind='canonical-prefix',command=command,native_stdout_index=len(worker.stages)+1+i,command_receipt=receipt))
        response,roundtrip=Current.call(worker,command)
        received=elapsed()
        normalized,observed=canonical_answer(i,response,manifest,expected=expected)
        expected=observed
        validated=elapsed()
        private=worker.rows[i]
        if private is not None and private['normalized_answer']!=normalized:
            raise ValueError('canonical P differs from complete private answer')
        fallback=private is None
        row=dict(command=command,answer=copy.deepcopy(response),normalized_answer=normalized,command_receipt=receipt,
            parent_requested_ns=requested,parent_received_ns=received,parent_validated_ns=validated,
            round_trip_ns=roundtrip,host_ns=response['host_ns'],validation_ns=validated-received,
            parent_elapsed_ns=validated-requested,counters=copy.deepcopy(response['counters']),
            resources_after_validation=resources(worker.process.pid),verified=True,
            dispatch='Host after Ready fallback' if fallback else 'Host after Ready canonical revalidation')
        prefix['rows'].append(row)
        worker.physical.append(dict(operation=('canonical_get_' if i==0 else 'canonical_find_')+('fallback' if fallback else 'revalidation'),dispatch='Host after Ready',command=command))
    worker.rows=resolve_rows(worker.rows,prefix['rows'])
    prefix.update(parent_end_ns=elapsed(),resources_after=resources(worker.process.pid))
    prefix['elapsed_ns']=prefix['parent_end_ns']-prefix['parent_start_ns']
    return prefix


def validate_slice(root,prefix,responses,stages,ready,history,manifest,audit,native_stage):
    """Consume the exact canonical P2, retaining the complete 481-call suffix validator."""
    from native_validation import validate_provenance
    load=lambda name:json.loads((root/name).read_text())
    from public_mode import (MODE as expected_mode, validate_ready as check_ready,
        validate_history as check_history, account as physical_account,
        public_answer as attempt_answer, logical_available as available)
    if prefix.get('mode')!=expected_mode or len(stages)!=2 or len(responses)!=483:
        raise ValueError('named live slice/native count differs')
    coverage=check_ready(stages,ready,manifest)
    attempts=load('loading-checkpoints.json')
    if prefix.get('attempts')!=attempts or [native_stage(s) for s in attempts]!=stages or load('ready-validation.json')!=coverage:
        raise ValueError('live native attempt/Ready receipts differ')
    check_history(attempts,ready,integer(prefix.get('parent_ready_received_ns')))
    expected_account=physical_account(attempts,ready)
    if prefix.get('physical_account')!=expected_account:raise ValueError('live physical account differs')
    provenance=validate_provenance(root,history,responses[2:],audit)
    expected=(provenance['store_epoch'],provenance['initial_generation'])
    canonical=prefix.get('canonical_revalidation',{})
    if (set(canonical)!={'slice','commands','rows','parent_start_ns','parent_end_ns','resources_before','resources_after','elapsed_ns'}
            or canonical.get('slice')!=SLICE or canonical.get('commands')!=list(COMMANDS) or len(canonical.get('rows',[]))!=2):
        raise ValueError('missing named canonical P2')
    validate_resources(canonical['resources_before']);validate_resources(canonical['resources_after'])
    previous=integer(canonical['parent_start_ns'])
    if previous<prefix['parent_ready_received_ns']:raise ValueError('canonical P2 precedes Ready')
    ledger=[];physical=[]
    for i,event in enumerate(stages):
        details=dict(disposition=event['disposition'])
        ledger.append(dict(kind='public-stage',command=COMMANDS[i],native_stdout_index=i,**details))
        physical.append(dict(operation=('get_attempt','find_attempt')[i],dispatch='public',command=COMMANDS[i],**details))
    for i,(command,response,row) in enumerate(zip(COMMANDS,responses[:2],canonical['rows'])):
        receipt=dict(slice=SLICE,logical_request=i+1,attempt='canonical-prefix',command=command,
            native_stdout_index=3+i,cell_token=(root/'performance-token').read_text(),root_identity=root_identity(root))
        if row.get('command_receipt')!=receipt or load(f'canonical-command-{i+1}.json')!=receipt or row.get('command')!=command or row.get('answer')!=response or row.get('verified') is not True:
            raise ValueError('canonical P2 differs from native command/answer/root')
        validate_resources(row['resources_after_validation'])
        normalized,_=canonical_answer(i,response,manifest,expected=expected)
        private=attempt_answer(i,stages[i],manifest)
        fallback=not available(stages[i])
        if stages[i].get('answer') is not None and stages[i]['answer']['reading']['trust']=={'state':'ready'}:
            reading=stages[i]['answer']['reading']
            if (reading['epoch'],reading['generation'])!=expected:
                raise ValueError('Ready public provenance differs from canonical P2')
        if private is not None and private!=normalized:raise ValueError('executed typed answer differs from canonical P')
        requested,received,validated=(integer(row.get(k)) for k in ('parent_requested_ns','parent_received_ns','parent_validated_ns'))
        dispatch='Host after Ready fallback' if fallback else 'Host after Ready canonical revalidation'
        if (not previous<=requested<=received<=validated or integer(row.get('round_trip_ns'))>received-requested
                or row.get('validation_ns')!=validated-received or row.get('parent_elapsed_ns')!=validated-requested
                or row.get('normalized_answer')!=normalized or row.get('counters')!=response['counters']
                or row.get('host_ns')!=response['host_ns'] or row.get('dispatch')!=dispatch):
            raise ValueError('canonical P2 content/counters/timing differs')
        logical=prefix['rows'][i]
        if fallback:
            if logical!=row:raise ValueError('unavailable logical answer needs exactly its canonical P fallback')
        elif (logical.get('answer')!=attempts[i] or logical.get('normalized_answer')!=private
                or logical.get('parent_validated_ns')!=attempts[i]['parent_validated_ns']):
            raise ValueError('first logical answer differs from validated native live answer')
        if prefix.get(('first_get_validated_ns','first_find_validated_ns')[i])!=logical['parent_validated_ns']:
            raise ValueError('first complete logical endpoint differs')
        previous=validated
        ledger.append(dict(kind='canonical-prefix',command=command,native_stdout_index=3+i,command_receipt=receipt))
        physical.append(dict(operation=('canonical_get_' if i==0 else 'canonical_find_')+('fallback' if fallback else 'revalidation'),dispatch='Host after Ready',command=command))
    if prefix.get('physical_history')!=physical:raise ValueError('physical P history differs')
    if canonical['parent_end_ns']<previous or canonical['elapsed_ns']!=canonical['parent_end_ns']-canonical['parent_start_ns']:
        raise ValueError('canonical P end differs')
    ledger += [dict(kind='canonical-suffix',command=c,native_stdout_index=5+i) for i,c in enumerate(history)]
    actual=[json.loads(line) for line in (root/'physical-history.jsonl').read_text().splitlines()]
    if ledger!=actual:raise ValueError('exact physical request ledger differs')
    common=load('common-search-endpoint.json')
    if common.get('parent_origin_monotonic_ns')!=prefix['parent_origin_monotonic_ns'] or common.get('command')!=history[0] or common.get('native_stdout_index')!=5:
        raise ValueError('common search origin/command differs')
    return responses[2:]


def validate_main_slice(root,prefix,responses,stages,ready,history,manifest,audit):
    from native_validation import validate_provenance
    from full_intake_control import validate_mode
    from reference import validate_reference_work, ARMS
    if stages or len(responses)!=483 or prefix.get('mode')!='main-ordinary':
        raise ValueError('ordinary main canonical P2 count differs')
    coverage=validate_mode('main-ordinary',[],ready,manifest)
    validate_reference_work(ARMS[0],ready,manifest)
    if json.loads((root/'ready-validation.json').read_text())!=coverage:raise ValueError('main Ready receipt differs')
    provenance=validate_provenance(root,history,responses[2:],audit)
    expected=(provenance['store_epoch'],provenance['initial_generation'])
    canonical=prefix['canonical_revalidation']
    if canonical['commands']!=list(COMMANDS) or canonical['slice']!=SLICE or len(canonical['rows'])!=2:
        raise ValueError('main ordinary P2 differs')
    ledger=[]
    for i,(response,row) in enumerate(zip(responses[:2],canonical['rows'])):
        normalized,_=canonical_answer(i,response,manifest,expected=expected)
        receipt=dict(slice=SLICE,logical_request=i+1,attempt='canonical-prefix',command=COMMANDS[i],native_stdout_index=1+i,
            cell_token=(root/'performance-token').read_text(),root_identity=root_identity(root))
        if (row['answer']!=response or row['command']!=COMMANDS[i] or row['normalized_answer']!=normalized
                or row!=prefix['rows'][i] or row['command_receipt']!=receipt
                or json.loads((root/f'canonical-command-{i+1}.json').read_text())!=receipt):
            raise ValueError('main prefix receipt differs from exact native P2')
        ledger.append(dict(kind='canonical-prefix',command=COMMANDS[i],native_stdout_index=1+i,command_receipt=receipt))
    ledger += [dict(kind='canonical-suffix',command=c,native_stdout_index=3+i) for i,c in enumerate(history)]
    if ledger!=[json.loads(s) for s in (root/'physical-history.jsonl').read_text().splitlines()]:
        raise ValueError('main exact physical history differs')
    return responses[2:]
