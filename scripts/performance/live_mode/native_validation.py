"""Recheck retained native warm responses using the accepted health-once oracles."""
import base64
import copy
import json
import re
import stat

from results import (whole_get_answer, validate_read, validate_first_get,
                     validate_write, record)


def nonnegative_generation(value):
    if type(value) is not int or not 0 <= value <= 2**64 - 1:
        raise ValueError('missing or invalid provenance generation')
    return value


def provenance_epoch(value):
    # AnswerReading carries an opaque String, not a fixed-width hex number.
    if not isinstance(value, str) or not value:
        raise ValueError('missing or invalid provenance epoch')
    return value


def validate_cursor_provenance(report, epoch, generation, index):
    page = report.get('page')
    if page is None:
        return
    if not isinstance(page, dict):
        raise ValueError(f'invalid native page at response {index}')
    token = page.get('next')
    if token is None:
        return
    try:
        if not isinstance(token, str) or not token:
            raise ValueError('invalid cursor token')
        cursor = json.loads(base64.b64decode(token + '=' * (-len(token) % 4),
                                            altchars=b'-_', validate=True))
        snapshot = cursor['snapshot']
        if (provenance_epoch(snapshot['epoch']) != epoch
                or nonnegative_generation(snapshot['generation']) != generation):
            raise ValueError('cursor snapshot differs from answer reading')
    except (ValueError, TypeError, KeyError) as problem:
        raise ValueError(f'invalid cursor provenance at response {index}') from problem


def validate_provenance(root, history, responses, audit, *, workload=None):
    """Bind the exact private, no-recovery history to its post-close store audit."""
    if workload is None:
        from warm_compare import validate_warm_history
        validate_warm_history(history)
        expected_responses, expected_writes = 481, 120
    else:
        if history != workload:
            raise ValueError('native history differs from the declared workload')
        expected_responses = len(workload)
        expected_writes = sum(command['op'] in ('set', 'new', 'move') for command in workload)
    if len(responses) != expected_responses or not isinstance(audit, dict):
        raise ValueError('incomplete native provenance or final audit')
    epoch = provenance_epoch(audit.get('store_epoch'))
    final_generation = nonnegative_generation(audit.get('write_generation'))
    writes = sum(command['op'] in ('set', 'new', 'move') for command in history)
    if writes != expected_writes or final_generation < writes:
        raise ValueError('final audit cannot account for the applied mutations')
    generation = final_generation - writes
    initial_generation = generation
    vault = (root / 'vault').stat()
    if not stat.S_ISDIR(vault.st_mode):
        raise ValueError('native provenance requires the actual vault directory')
    # Pinned norn-wire plan/root.rs constructor. Do not parse a wire identity
    # into parts or derive the expected identity from the first native plan.
    expected_root = f'{vault.st_dev:016x}{vault.st_ino:016x}'
    for index, (command, response) in enumerate(zip(history, responses)):
        if not isinstance(response, dict) or set(response) != {'answer', 'counters', 'host_ns'}:
            raise ValueError(f'native response envelope differs at response {index}')
        answer = response.get('answer')
        reading = answer.get('reading') if isinstance(answer, dict) else None
        if not isinstance(reading, dict):
            raise ValueError(f'missing answer provenance at response {index}')
        if set(reading) != {'trust', 'epoch', 'generation'}:
            raise ValueError(f'unexpected answer reading fields at response {index}')
        if (reading.get('trust') != {'state': 'ready'}
                or provenance_epoch(reading.get('epoch')) != epoch
                or nonnegative_generation(reading.get('generation')) != generation):
            raise ValueError(f'answer provenance differs from exact cell history at response {index}')
        report = answer.get('report')
        if not isinstance(report, dict):
            raise ValueError(f'missing native report at response {index}')
        validate_cursor_provenance(report, epoch, generation, index)
        writing = command['op'] in ('set', 'new', 'move')
        if writing or 'plan' in report:
            plan = report.get('plan')
            identity = plan.get('root') if isinstance(plan, dict) else None
            if (not isinstance(identity, str) or re.fullmatch('[0-9a-f]{32}', identity) is None
                    or identity != expected_root):
                raise ValueError(f'plan root differs from actual vault identity at response {index}')
        if writing:
            validate_write('current', response)
            # production.rs apply_over retains its pre-apply StoreReading;
            # lifecycle.rs wraps that reading after the applier finishes.
            # increment.rs takes one generation
            # for the whole changeset, including a multi-file hub rename.
            generation += 1
    if generation != final_generation:
        raise ValueError('native history does not reach the final audited generation')
    return dict(store_epoch=epoch, initial_generation=initial_generation,
                final_generation=final_generation, responses=expected_responses, applied_mutations=writes,
                root_identity=expected_root)


def norm(resp):
 a=copy.deepcopy(resp['answer']);rd=a.pop('reading')
 if (not isinstance(rd,dict) or rd.get('trust')!={'state':'ready'}):
  raise ValueError('native normalization requires Ready provenance')
 provenance_epoch(rd.get('epoch'));nonnegative_generation(rd.get('generation'))
 if a['report'].get('shape')=='record':return whole_get_answer(resp)
 page=a['report'].get('page')
 if page and page.get('next'):
  token=page['next'];v=json.loads(base64.urlsafe_b64decode(token+'='*(-len(token)%4)));ss=v['snapshot']
  if ss.pop('epoch')!=rd['epoch'] or ss.pop('generation')!=rd['generation']:
   raise ValueError('native cursor snapshot differs from its reading')
  page['next']=v
 plan=a['report'].get('plan')
 if plan:plan.pop('root') # per-private-root filesystem identity only
 return a


def validate_native_cell(root, records, manifest, audit=None, *, mode=None):
    """Check every native response, full sample account, and confirming Get."""
    from baseline import READS, WRITES, mutation
    from transport import PREFIX
    from warm_compare import validate_warm_history, summarize
    history = [json.loads(line) for line in (root / 'request-history.jsonl').read_text().splitlines()]
    validate_warm_history(history)
    summarize(records)
    transcript = []
    for line in (root / 'worker.stdout.log').read_text().splitlines():
        if not line.startswith(PREFIX):
            raise ValueError('unexpected native protocol output')
        transcript.append(json.loads(line[len(PREFIX):]))
    ready_positions = [i for i,row in enumerate(transcript) if row.get('ready') is True]
    if len(ready_positions) != 1: raise ValueError('native Ready history differs')
    at = ready_positions[0]
    stages, ready = transcript[:at], transcript[at]
    if ready != records[0]['ready'] or any('startup_stage' not in row for row in stages):
        raise ValueError('incomplete or reordered native Ready/stage history')
    if audit is None:
        audit = json.loads((root / 'final-audit.json').read_text())
    responses = transcript[at+1:]
    prefix = json.loads((root/'prefix.json').read_text())
    if len(prefix['rows']) != 2: raise ValueError('exactly two retained prefix receipts required')
    from revalidation import native_stage
    from public_mode import MODE as PUBLIC_MODE
    if mode == PUBLIC_MODE:
        from public_mode import validate_slice as validate_public_slice
        responses = validate_public_slice(root,prefix,responses,stages,ready,history,manifest,audit,native_stage)
    elif mode == 'main-ordinary':
        from revalidation import validate_main_slice
        responses = validate_main_slice(root,prefix,responses,stages,ready,history,manifest,audit)
    else:
        raise ValueError('public comparison requires its explicit named mode')
    if len(responses) != 481: raise ValueError('canonical response count differs after exact prefix slice')
    if stages:
        observed = json.loads((root/'loading-checkpoints.json').read_text())
        if len(observed) != len(stages):
            raise ValueError('native checkpoint count differs')
        for index, (row, native) in enumerate(zip(observed, stages)):
            if native_stage(row) != native:
                raise ValueError(f'native checkpoint {index} differs from actual native stage')
    if ([row['kind'] for row in records] != ['startup'] +
            (['sample'] * 30 + ['resources']) * 12 + ['end']):
        raise ValueError('incomplete or reordered warm sample lifecycle')
    if [row['scenario'] for row in records if row['kind'] == 'resources'] != [name for name, _ in READS] + WRITES:
        raise ValueError('missing operation resource boundaries')
    if audit is None:
        audit = json.loads((root / 'final-audit.json').read_text())
    # Establish cell provenance before the inherited cross-cell normalization.
    validate_provenance(root, history, responses, audit)
    normalized = []
    for response in responses:
        counters = response.get('counters')
        if not isinstance(counters, dict):
            raise ValueError('native response lacks its counter dictionary')
        normalized.append(dict(answer=norm(response), counters=counters))
    validate_read('current', history[0], responses[0], manifest)
    if json.loads((root / 'first-query.json').read_text()) != responses[0]:
        raise ValueError('common-search preparation receipt differs from native response')
    samples = [row for row in records if row['kind'] == 'sample']
    state = {name: dict(path=name, body=body, fields={'status':'draft','perf_bucket':'probe'})
             for name, body in manifest['probe_bodies'].items()}
    position = 1
    for operation, base in READS + [(name, None) for name in WRITES]:
        rows = [row for row in samples if row['scenario'] == operation]
        for row in rows:
            command = base or mutation(operation, row['iteration'])
            response = responses[position]
            if (row['command'] != command or history[position] != command
                    or row['counters'] != response['counters'] or row['host_ns'] != response['host_ns']
                    or row['advisories'] != response['answer']['advisories'] or row['advisories'] != []
                    or row['legacy_warnings'] != []):
                raise ValueError('sample command, counters or diagnostics differ from native response')
            position += 1
            if base is not None:
                validate_read('current', command, response, manifest)
                if operation == 'get_small':
                    validate_first_get(response, manifest)
                if any(row[key] is not None for key in ('readback_ns','readback_diagnostics','write_and_readback_ns')):
                    raise ValueError('read operation has unexpected readback data')
            else:
                validate_write('current', response)
                target = {'set':'bench/subject.md','new':command.get('path'),'move':command.get('to')}[command['op']]
                readback = responses[position]
                if (history[position] != {'op':'get','target':target}
                        or readback['answer']['advisories'] != row['readback_diagnostics']
                        or row['readback_diagnostics'] != []
                        or type(row['readback_ns']) is not int or row['readback_ns'] < 0
                        or row['write_and_readback_ns'] != row['round_trip_ns'] + row['readback_ns']):
                    raise ValueError('confirming Get history or diagnostics differ')
                if command['op'] == 'set':
                    expected = {**state[target], 'fields':{**state[target]['fields'],'status':command['value']}}
                elif command['op'] == 'new':
                    expected = dict(path=target, body=command['body'], fields={'status':'draft'})
                else:
                    expected = {**state.pop(command['from']), 'path':target}
                if record('current', readback) != expected:
                    raise ValueError('confirming Get differs from authored mutation')
                state[target] = expected
                position += 1
    if position != 481:
        raise ValueError('native warm response coverage differs')
    return normalized
