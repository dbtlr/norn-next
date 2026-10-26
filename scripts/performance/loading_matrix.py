#!/usr/bin/env python3
"""Serial, one-process loading cells using the existing performance adapters."""
import argparse
import json
import sqlite3
from contextlib import closing
from pathlib import Path

from loading_method import (ARMS, LINK_TARGET, BASE_2014, plan, gate_semantics, identity_map,
                            store_layout, await_idle, BUSY_THRESHOLDS, summarize)

MAIN_BASE = '2e213c09'
REVISIONS = {'main': 'perf/norn-459-main-worker', 'health-once': 'e944a338'}
PUBLIC_GET_TARGETS = ['small', LINK_TARGET]
PUBLIC_COMMANDS = [dict(op='get', target=target, whole=True) for target in PUBLIC_GET_TARGETS] + [dict(op='find')]


def mid_request_target(manifest):
    paths = sorted(manifest['paths'])
    index = len(paths) * 9 // 10
    if index < 128:
        raise ValueError('mid-request target must be outside the first 128-path batch')
    target = paths[index]
    if target in ('bench/small.md', LINK_TARGET):
        raise ValueError('mid-request target must differ from start-requested paths')
    return target


def validate_loading_flags(stage):
    if (stage.get('link_resolution_loading') is not True
            or stage.get('link_health_loading') is not True):
        raise ValueError('early lab Get requires explicit link-resolution and link-health loading flags')


def validate_mid_get(response, manifest, *, first_index=False):
    from results import whole_get_answer
    whole_get_answer(response, first_index=first_index)
    document = response['answer']['report']['document']
    body = manifest['mid_request_body']
    if (document['path'] != manifest['mid_request_path']
            or document['body'] != {'text': body, 'byte_length': len(body.encode())}):
        raise ValueError('mid-request Get differs from the frozen source body')


def compare_own_structure(early, final):
    for key in ('path', 'body', 'fields', 'headings', 'blocks', 'tags'):
        if early[key] != final[key]:
            raise ValueError('lab Get own content/structure differs from public Get')
    stable_links = lambda document: [{key: value for key, value in link.items() if key not in ('health', 'targets')}
                                      for link in document['links']['items']]
    if stable_links(early) != stable_links(final):
        raise ValueError('lab Get authored links differ from public Get')


def mid_request_seeds(reference, fts_mode):
    metadata = json.loads((reference / 'run.json').read_text())
    if metadata.get('completed') is not True or metadata.get('semantic_equal') is not True:
        raise ValueError('mid-request reference must be a completed, gated lab run')
    seeds = {}
    for path in metadata['results']:
        result = json.loads(Path(path).read_text())
        if result['arm'] != 'four-step':
            continue
        if result.get('fts_mode') != fts_mode:
            raise ValueError('mid-request reference FTS mode differs')
        mid = result['mid_request']
        if type(mid['request_ns']) is not int or mid['request_ns'] < 0:
            raise ValueError('invalid mid-request reference offset')
        seeds.setdefault(result['document_count'], dict(path=mid['target'], offset_ns=mid['request_ns'],
                                                        fixture_sha256=result['fixture_sha256']))
    return seeds


def selected_cells(sizes, arms, repetitions=None, skip_warmup=False, row_start=0):
    return [cell for cell in plan(sizes, repetitions, row_start)
            if (cell['discarded_warmup'] and not skip_warmup)
            or (not cell['discarded_warmup'] and cell['arm'] in arms)]


def verify_store(binary, root, phase):
    import subprocess
    import time
    from startup_audit import messages
    from transport import TIMEOUT, environment, stop_process
    token = (root / 'performance-token').read_text()
    deadline = time.monotonic() + TIMEOUT
    with (root / f'integrity-{phase}.stdout.log').open('wb') as raw, \
            (root / f'integrity-{phase}.stderr.log').open('wb') as errors:
        process = subprocess.Popen([str(binary), 'verify'], cwd=root, env=environment(root, token),
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=errors, bufsize=0, start_new_session=True)
        try:
            records = list(messages(process, raw, deadline))
            process.wait(timeout=max(0.001, deadline-time.monotonic()))
            if process.returncode:
                raise RuntimeError(f'native integrity verification failed at {phase}; inspect integrity logs')
            if records != [{'integrity_verified': True}]:
                raise ValueError('native integrity verifier supplied no unique positive receipt')
        finally:
            stop_process(process)
            process.stdout.close()
    return dict(integrity_verified=True, binary=str(binary), phase=phase,
                scope='untimed store-owned integrity check including FTS')


def documents_layout(path):
    """Inspect only the closed documents B-tree; index and freelist pages are excluded."""
    with closing(sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True)) as db:
        rows = db.execute("select path,pageno,pagetype,payload,unused,pgsize from dbstat "
                          "where name='documents' order by path").fetchall()
    if not rows:
        raise ValueError('dbstat supplied no documents pages')
    # Logical B-tree traversal order versus physical page adjacency, including overflow chains.
    breaks = sum(b[1] != a[1] + 1 for a, b in zip(rows, rows[1:]))
    return dict(page_count=len(rows), overflow_pages=sum(r[2] == 'overflow' for r in rows),
                average_fill=sum(r[3] for r in rows) / sum(r[5] for r in rows),
                unused_bytes=sum(r[4] for r in rows), fragmentation=breaks / max(1, len(rows) - 1),
                nonadjacent_transitions=breaks,
                definitions={'average_fill': 'sum(payload)/sum(pgsize), all documents B-tree pages',
                             'fragmentation': 'fraction of successive dbstat path-order pages whose pageno is not previous+1',
                             'scope': 'post-warm writes, after worker close and final audit; documents table interior, leaf and overflow pages'})


def validate_lab_stage(stage, manifest):
    """Validate the real hook answer against the existing frozen fixture oracle."""
    from results import validate_read
    name = stage['startup_stage']
    if name not in ('four-step-get', 'four-step-find', 'four-step-search'):
        return
    if stage.get('current') is not True:
        raise ValueError('lab hook lost its current source')
    report = stage['answer']
    wrapped = {'answer': {'report': report, 'unsatisfied': stage['unsatisfied'],
                           'advisories': stage.get('advisories', [])}}
    if name == 'four-step-get':
        validate_loading_flags(stage)
        if stage.get('path') not in ('bench/small.md', LINK_TARGET, manifest.get('mid_request_path')):
            raise ValueError('lab hook answered the wrong target')
        if stage['path'] == LINK_TARGET:
            wrapped.update(projection_epoch=stage['snapshot']['epoch'],
                           projection_generation=stage['snapshot']['generation'])
            validate_link_get(wrapped, manifest, health_loading=True, resolution_loading=True)
        elif stage['path'] == manifest.get('mid_request_path'):
            wrapped.update(projection_epoch=stage['snapshot']['epoch'], projection_generation=stage['snapshot']['generation'])
            validate_mid_get(wrapped, manifest)
        else:
            validate_read('current', {'op': 'get', 'target': 'bench/small.md'}, wrapped, manifest)
        document = report['document']
        for key in ('headings', 'blocks', 'tags', 'links', 'findings'):
            collection = document[key]
            if collection['total'] != len(collection['items']):
                raise ValueError('lab whole Get truncated a collection')
    elif name == 'four-step-find':
        validate_read('current', {'op': 'find'}, wrapped, manifest)
        page = report['page']
        if (page['moved'] or (page['next'] is not None) != (len(manifest['active_paths']) > 25)
                or any(set(row) != {'path'} for row in page['rows'])):
            raise ValueError('lab metadata page differs from the public path-only page')
    else:
        from fixture import COMMON
        if stage.get('query') != COMMON:
            raise ValueError('lab Search used the wrong query')
        validate_read('current', {'op': 'search', 'query': COMMON}, wrapped, manifest)
        page = report['page']
        if (report['ladder'] != {'rungs': [{'rung': 'lexical'}], 'repeatable': True}
                or page['moved'] or (page['next'] is not None) != (len(manifest['paths']) > 25)):
            raise ValueError('lab Search differs from the lexical first page')


def validate_link_get(response, manifest, health_loading=False, *, first_index=False, resolution_loading=False):
    """Validate the authored whole record, including each link and finding handle."""
    from results import whole_get_answer
    if first_index:
        whole_get_answer(response, first_index=True)
    else:
        whole_get_answer(response)
    doc = response['answer']['report']['document']
    body = manifest['large_body']
    source = '---\nstatus: draft\nperf_bucket: probe\n---\n' + body

    def span(raw):
        offset = source.index(raw)
        return dict(byte_offset=len(source[:offset].encode()), line=source[:offset].count('\n') + 1,
                    column=offset - source.rfind('\n', 0, offset))

    expected_fields = {key: {'kind': 'scalar', 'raw': value}
                       for key, value in {'status': 'draft', 'perf_bucket': 'probe'}.items()}
    if (doc['path'] != LINK_TARGET or doc['body'] != {'text': body, 'byte_length': len(body.encode())}
            or doc['fields'] != expected_fields):
        raise ValueError('linked Get content differs from fixture')
    headings = [dict(level=level, slug=slug, text=text, span=span(raw))
                for level, slug, text, raw in [(1, 'large', 'Large', '# Large'),
                    (2, 'details', 'Details', '## Details'), (2, 'tail', 'Tail', '## Tail')]]
    if (doc['headings']['items'] != headings or doc['blocks']['items'] or doc['tags']['items']):
        raise ValueError('linked Get structure differs from fixture')
    expected_links = []
    for family, target, title, anchor, raw in [
            ('wikilink', 'small', None, {'kind': 'heading', 'text': 'Details'}, '[[small#Details]]'),
            ('markdown', 'small.md', 'Small', {'kind': 'heading', 'text': 'Details'}, '[Small](small.md#Details)'),
            ('wikilink', 'matrix-missing-target', None, None, '[[matrix-missing-target]]')]:
        found = target != 'matrix-missing-target'
        expected_links.append(dict(family=family, target=target, title=title, anchor=anchor, span=span(raw),
                                   embed=False, protocol=None, health='healthy' if found else 'broken',
                                   targets={'candidates': [{'path': 'bench/small.md', 'suffix': 'small'}] if found else [],
                                            'total': int(found)}))
    actual_links = doc['links']['items']
    if resolution_loading:
        stable = lambda link: {key: value for key, value in link.items() if key not in ('health', 'targets')}
        actual_links, expected_links = [stable(link) for link in actual_links], [stable(link) for link in expected_links]
    if actual_links != expected_links:
        raise ValueError('linked Get targets, anchors, spans, or resolutions differ from fixture')
    findings = doc['findings']['items']
    if health_loading:
        if findings:
            raise ValueError('lab Get unexpectedly includes step-4 findings')
    else:
        if len(findings) != 1:
            raise ValueError('public linked Get omitted broken-link finding')
        finding = findings[0]
        expected = dict(path=LINK_TARGET, kind='link/broken', severity='warning',
                        target='matrix-missing-target', span=span('[[matrix-missing-target]]'),
                        head={'candidates': [], 'total': 0}, message='the link names no document',
                        hint=None, rule_set=None, value=None)
        if {key: value for key, value in finding.items() if key not in ('id', 'generation')} != expected:
            raise ValueError('public linked Get broken-link finding differs from fixture')


def endpoint(origin, *, path, response=None, worker_ns=None, **fields):
    import time
    return dict(parent_received_ns=time.perf_counter_ns() - origin, worker_ns=worker_ns,
                path=path, response=response, **fields)


def describe_probe(probe, arm, kind):
    lab = arm.startswith('four-step')
    result = dict(probe, timing_scope=('actual lab hook query and typed-answer delivery' if lab else
                                     'first validated public answer; includes admission and builder query time'),
                  delivery_scope=('buffered in worker; serialized and delivered after build' if lab else
                                  'public response delivered when answered'))
    if lab:
        available, started, finished, duration, delivered = (probe[key] for key in (
            'earliest_available_ns', 'query_started_ns', 'query_finished_ns', 'query_host_ns', 'delivery_ns'))
        if (any(type(value) is not int for value in (available, started, finished, duration, delivered))
                or not 0 <= available <= started <= finished or duration != finished - started
                or delivered != finished):
            raise ValueError('invalid lab query timing')
        result.update(worker_ns=delivered,
                      worker_clock_scope='actual typed-answer delivery on worker clock',
                      availability_clock_scope='prerequisite completion observed on worker clock')
    if kind == 'get':
        result['findings_scope'] = ('link resolution loading; link health loading' if lab else
                                    'includes link-health findings')
    return result


def lab_measurements(arm, events):
    names = [event['startup_stage'].removeprefix('four-step-') for event in events]
    expected = ['names', 'content', 'find', 'structure', 'relations', 'full-text', 'search', 'complete']
    if [name for name in names if name not in ('request', 'requested-structure', 'get')] != expected:
        raise ValueError('four-step boundary/probe coverage differs')
    requested = {event['path']: event for event in events if event['startup_stage'] == 'four-step-request'}
    structured = {event['path']: event for event in events if event['startup_stage'] == 'four-step-requested-structure'}
    gets = {event['path']: event for event in events if event['startup_stage'] == 'four-step-get'}
    for kind in ('request', 'requested-structure', 'get'):
        if sum(name == kind for name in names) != len({'request': requested, 'requested-structure': structured, 'get': gets}[kind]):
            raise ValueError('duplicate four-step target observation')
    if not requested or set(requested) != set(structured) or set(requested) != set(gets):
        raise ValueError('four-step requested target coverage differs')
    stages = {event['startup_stage']: event for event in events}
    report = stages['four-step-complete']
    if report['reconciled'] is not False:
        raise ValueError('source churn required an ordinary heal during the lab build')
    if report.get('fts_mode') not in ('rebuild', 'batched'):
        raise ValueError('four-step FTS mode missing')
    steps = ('names', 'content', 'structure', 'relations', 'full-text')
    boundaries, busy = report['boundary_ns'], report['busy_ns']
    if any(set(mapping) != set(steps) for mapping in (boundaries, busy, report['steps'])):
        raise ValueError('four-step timing/counter coverage differs')
    intervals, previous = {}, 0
    for name in steps:
        boundary = boundaries[name]
        if type(boundary) is not int or boundary < previous or type(busy[name]) is not int or busy[name] < 0:
            raise ValueError('invalid four-step timing')
        intervals[name] = boundary - previous
        previous = boundary
    pull_ahead = report['pull_ahead_ns']
    if type(pull_ahead) is not int or pull_ahead < 0:
        raise ValueError('invalid four-step pull-ahead timing')
    build_times = [report[key] for key in ('request_ns', 'requested_structure_ns', 'delivery_ns')]
    if any(set(mapping) != set(requested) for mapping in build_times):
        raise ValueError('four-step build request timing coverage differs')
    latencies = {}
    for path in requested:
        request_ns, structure_ns, delivery_ns = [mapping[path] for mapping in build_times]
        if (any(type(value) is not int for value in (request_ns, structure_ns, delivery_ns))
                or not 0 <= request_ns <= structure_ns <= delivery_ns):
            raise ValueError('invalid four-step build request timing')
        latencies[path] = delivery_ns - request_ns
    mid = [event for event in requested.values() if event['request_kind'] == 'mid-content']
    if len(mid) != 1:
        raise ValueError('exactly one mid-content request is required')
    mid = mid[0]
    path, progress = mid['path'], mid['content_progress']
    if (not 0 < progress['completed'] <= progress['total']
            or progress['completed'] * 4 < progress['total']):
        raise ValueError('mid-content request must follow the 25% batch threshold')
    request_ns = mid['request_ns']
    structure_ns = structured[path]['elapsed_ns']
    delivery_ns = gets[path]['delivery_ns']
    if not 0 <= request_ns <= structure_ns <= delivery_ns:
        raise ValueError('invalid worker mid-request timing')
    validate_loading_flags(gets[path])
    result = dict(step_counters=report['steps'], step_boundary_ns=boundaries,
                  step_boundary_intervals_ns=intervals, step_busy_ns=busy, pull_ahead_ns=pull_ahead,
                  fts_mode=report['fts_mode'], build_request_ns=report['request_ns'],
                  build_requested_structure_ns=report['requested_structure_ns'], build_delivery_ns=report['delivery_ns'],
                  build_request_to_delivery_ns=latencies,
                  worker_request_ns={path: event['request_ns'] for path, event in requested.items()},
                  step_boundary_scope='lab build origin; end of each main pass before observer queries; relations includes authority check; full-text includes FTS and finish teardown',
                  step_busy_scope='main-pass elapsed work excluding observer queries and requested-path pull-ahead; structure includes timing bookkeeping',
                  pull_ahead_scope='requested-path content and structure elapsed work; excludes observer queries',
                  step_duration_scope='boundary intervals include intervening observers and pull-ahead; busy_ns is elapsed work, not CPU time',
                  counter_scope={'files_read': 'source-read attempts, including failed opens and paths refused before opening',
                      'rows_rewritten': 'content/metadata transitions; separate no-body payload updates are body_rows_rewritten',
                      'fts': 'step-5 fts_rows_inserted counts indexed document rows; SQLite shadow-table work is uncounted'},
                  content_complete=stages['four-step-content'],
                  mid_request=dict(target=path, request_ns=request_ns, requested_document_structure_ns=structure_ns,
                      delivery_ns=delivery_ns, request_to_delivery_ns=delivery_ns-request_ns,
                      request_alignment='boundary-aligned', current_batch_wait_included=False,
                      latency_scope='boundary-aligned; excludes current-batch wait; best-case relative to asynchronous arrival',
                      content_progress=progress, clock_scope='worker origin; separate from build report clocks',
                      comparison='not comparable (side by side, no ratios)',
                      delivery_scope='typed answer returned by lab hook; stdout buffered until build ends'))
    for field, name in [('ready', 'full-text'), ('health_complete', 'relations'),
                        ('requested_document_structure', 'requested-structure')]:
        event = next(event for event in events if event['startup_stage'] == 'four-step-' + name)
        result[field] = dict(parent_received_ns=event['parent_received_ns'], worker_ns=event['elapsed_ns'],
                             resources=event.get('resources_when_observed'), source=event['startup_stage'],
                             delivery_scope='buffered in worker; delivered after build',
                             resource_scope='sampled after build on buffered event receipt')
    result['requested_documents_structure'] = {path: dict(worker_ns=event['elapsed_ns'], parent_received_ns=event['parent_received_ns']) for path, event in structured.items()}
    result.update({key: report[key] for key in ('refused_ns', 'request_refusals', 'timing_commits', 'timing_ns') if key in report})
    result['timing_bookkeeping_scope'] = 'timing_commits and timing_ns are a bookkeeping subset of total commits and elapsed work; structure totals include them'
    result['structure_observation_scope'] = 'Structured callback observed after timestamp commit (conservative); build requested_structure_ns captured before timestamp commit'
    result['ready']['counters'] = result['step_counters']
    result['health_complete']['counters'] = {step: result['step_counters'][step] for step in steps[:4]}
    return result


def cell_summary(documents, result):
    ready = result['ready']
    boundary = ready.get('source', 'public Ready control record')
    worker_ready = ready.get('worker_ns')
    clock = f"worker {worker_ready / 1e9:.6f}s " if worker_ready is not None else ''
    lines = [f"{documents} {result['arm']}: Ready[{boundary}] "
             f"{clock}"
             f"public_Ready_worker {result['public_ready'].get('worker_ns', 0) / 1e9:.6f}s (health complete)"]
    for kind in ('get', 'find', 'search'):
        probe = result['first_' + kind]
        worker = probe.get('worker_ns')
        if 'delivery_ns' in probe:
            clock = f"actual_delivery {probe['delivery_ns'] / 1e9:.6f}s "
        else:
            clock = f"worker {worker / 1e9:.6f}s " if worker is not None else ''
        findings = '; ' + probe['findings_scope'] if kind == 'get' else ''
        lines.append(f"  {kind.title()}[{probe['path']}] {clock}"
                     f"query_host {probe.get('query_host_ns', probe.get('response', {}).get('host_ns', 0)) / 1e9:.6f}s; "
                     f"parent_delivery {probe['parent_received_ns'] / 1e9:.6f}s (not comparable); "
                     f"{probe['timing_scope']}{findings}")
    for target, probe in result.get('first_get_targets', {}).items():
        lines.append(f"  Get target {target}: own_step3 {probe.get('requested_document_structure_ns')}ns "
                     f"admitted {probe.get('earliest_available_ns')}ns query_host {probe.get('query_host_ns')}ns; {probe['path']}")
    if 'mid_request' in result:
        mid = result['mid_request']
        lines.append(f"  Mid Get[{mid['target']}] request {mid['request_ns']}ns own_step3 {mid.get('requested_document_structure_ns')}ns delivery {mid.get('delivery_ns')}ns request_to_delivery {mid.get('request_to_delivery_ns')}ns; {mid['comparison']}")
    lines.append(f"  semantic {result['store']['semantic_sha256']}")
    return '\n'.join(lines)


def measure_cell(binary, root, manifest, arm, *, fts_mode='rebuild', mid_request_offset_ns=None, integrity_binary=None):
    """One measured worker; validation and final audits use the existing harness code."""
    import os
    import time
    from baseline import READS, WRITES, mutation, verify_mutation, write_json
    from fixture import COMMON
    from results import validate_read
    if arm == 'health-once':
        from results import validate_first_get as validate_first_get_content
    else:
        from results import validate_first_get_content
    from startup_audit import audit_store
    from transport import Current, resources
    if arm == 'health-once':
        # In the isolated archive adapter process, this resolves to e944a338's module.
        from loading import validate_receipts
    from native_validation import validate_provenance

    events, probes, get_targets = [], {}, {}
    integrity = {}
    if arm.startswith('four-step'):
        integrity_binary = binary
    mid_target = mid_request_target(manifest) if 'paths' in manifest else None
    if mid_target:
        source = (root / 'vault' / mid_target).read_text()
        body = source.split('\n---\n', 1)[1] if source.startswith('---\n') else source
        manifest = dict(manifest, mid_request_path=mid_target, mid_request_body=body)
    mid_public = None
    origin = time.perf_counter_ns()

    def on_stage(stage):
        nonlocal mid_public
        stage['parent_received_ns'] = time.perf_counter_ns() - origin
        name = stage['startup_stage']
        if arm == 'main' and name == 'public-mid-get':
            if mid_public is not None or stage['path'] != mid_target or stage['requested_offset_ns'] != mid_request_offset_ns:
                raise ValueError('public mid-request identity differs')
            if not 0 <= stage['requested_offset_ns'] <= stage['request_ns'] <= stage['delivery_ns']:
                raise ValueError('invalid public mid-request timing')
            if stage['disposition'] == 'served':
                validate_mid_get(stage, manifest, first_index=True)
                get_targets[mid_target] = dict(parent_received_ns=stage['parent_received_ns'],
                    worker_ns=stage['delivery_ns'], path='public Host::get at prior lab wall offset', response=stage,
                    request_ns=stage['request_ns'], delivery_ns=stage['delivery_ns'],
                    query_started_ns=stage['query_started_ns'], query_finished_ns=stage['query_finished_ns'],
                    query_host_ns=stage['query_host_ns'])
            elif stage['disposition'] != 'unavailable' or stage['error'].get('code') != 'host/entry-not-ready':
                raise ValueError('public mid-request failed operationally')
            mid_public = dict(stage, target=stage['path'])
            events.append(stage)
            return
        if arm == 'main':
            from public_mode import validate_attempt
            index = sum('operator_prefix_sequence' in event for event in events)
            validate_attempt(index, stage, commands=PUBLIC_COMMANDS)
            if stage['disposition'] == 'served':
                kind = PUBLIC_COMMANDS[index]['op']
                if kind == 'get':
                    if index == 0:
                        validate_first_get_content(stage, manifest, first_index=True)
                    else:
                        validate_link_get(stage, manifest, first_index=True)
                else:
                    validate_read('current', {'op': 'find'}, stage, manifest)
                probe = dict(
                    parent_received_ns=stage['parent_received_ns'], worker_ns=stage['query_finished_ns'],
                    path=f'public Host::{kind}', response=stage,
                    query_started_ns=stage['query_started_ns'], query_finished_ns=stage['query_finished_ns'],
                    query_host_ns=stage['query_host_ns'])
                if kind == 'get':
                    get_targets[('bench/small.md', LINK_TARGET)[index]] = probe
                if index != 1:
                    probes[kind] = probe
            ack = root / 'operator-prefix-acks' / f'{index + 1}.pending'
            write_json(ack, dict(sequence=index + 1, probe=stage['probe']))
            ack.replace(ack.with_suffix('.json'))
        elif arm.startswith('four-step'):
            validate_lab_stage(stage, manifest)
            if name in ('four-step-get', 'four-step-find', 'four-step-search'):
                kind = name.removeprefix('four-step-')
                key = stage['path'] if kind == 'get' else kind
                target_probes = get_targets if kind == 'get' else probes
                if key in target_probes:
                    raise ValueError('duplicate lab probe')
                target_probes[key] = dict(parent_received_ns=stage['parent_received_ns'],
                    worker_ns=stage['query_finished_ns'], path=f'lab FourStepRead::{kind}', response=stage,
                    earliest_available_ns=stage['earliest_available_ns'], query_host_ns=stage['query_host_ns'],
                    query_started_ns=stage['query_started_ns'], query_finished_ns=stage['query_finished_ns'],
                    delivery_ns=stage['delivery_ns'],
                    **({key: stage[key] for key in ('link_resolution_loading', 'link_health_loading')} if kind == 'get' else {}))
                if kind == 'get' and stage['path'] == 'bench/small.md':
                    probes['get'] = target_probes[key]
        events.append(stage)

    if arm == 'main':
        (root / 'operator-prefix-acks').mkdir()
    loading = {'main': 'operator-prefix-public'}.get(arm, arm)
    # Explicit lab controls are supplied by the child process environment. Current
    # strips inherited NORN_* values, so the adapter's typed options own selection.
    options = dict(loading=loading, stage_handler=on_stage)
    if arm == 'main':
        options['public_get_targets'] = PUBLIC_GET_TARGETS
        if mid_request_offset_ns is not None:
            options['public_mid_request'] = dict(path=mid_target, offset_ns=mid_request_offset_ns)
    if arm.startswith('four-step'):
        options['loading_options'] = {'transaction_bound': 128, 'requested_paths': ['bench/small.md', LINK_TARGET],
                                      'fts_mode': fts_mode, 'mid_request_path': mid_target}
    worker = Current(binary, root, **options)
    history, observations = [], []
    try:
        public_ready = endpoint(origin, path='public Ready control record',
            worker_ns=worker.ready['observed_ready_ns'], counters=worker.ready['job_evidence'],
            resources=resources(worker.process.pid))
        if arm.startswith('four-step'):
            if worker.ready.get('loading_mode') != arm:
                raise ValueError('worker did not execute requested four-step mode')
            lab = lab_measurements(arm, events)
            if lab['step_counters']['full-text'].get('fts_rows_inserted') != manifest['document_count']:
                raise ValueError('step-5 FTS rows differ from the frozen document count')
        elif arm == 'health-once':
            validate_receipts(arm, events, worker.ready, manifest)
        elif (sum('operator_prefix_sequence' in event for event in events) != len(PUBLIC_COMMANDS)
              or worker.ready['operator_prefix_receipt']['logical_requests'] != len(PUBLIC_COMMANDS)):
            raise ValueError('main must execute all three public prefix attempts')
        if arm == 'main' and mid_request_offset_ns is not None and mid_public is None:
            raise ValueError('main must execute the timed mid-request')
        if arm.startswith('four-step') and lab['fts_mode'] != fts_mode:
            raise ValueError('worker did not execute requested FTS mode')

        # Untimed independent gate while the measured process is idle, before any
        # query or rename can conceal a step-4 health difference.
        post_ready_store = audit_store(binary, root, manifest)
        if integrity_binary is not None:
            integrity['post_ready'] = verify_store(integrity_binary, root, 'post_ready')
        database = next((root / 'data').rglob('store.sqlite3'))
        post_ready_path_ids = identity_map(database)
        post_ready_layout = store_layout(database)

        def call(command, *, name):
            requested = time.perf_counter_ns() - origin
            response, elapsed = worker.call(command)
            received = time.perf_counter_ns() - origin
            history.append(command)
            row = dict(name=name, command=command, response=response, round_trip_ns=elapsed,
                       parent_requested_ns=requested, parent_received_ns=received,
                       host_ns=response['host_ns'], counters=response['counters'])
            observations.append(row)
            return row

        for kind, command in [('get', {'op': 'get', 'target': 'small', 'whole': True}),
                              ('find', {'op': 'find'})]:
            row = call(command, name='canonical_' + kind)
            if kind == 'get':
                validate_first_get_content(row['response'], manifest)
                # The fixture has no small-document links. Verify all whole collections
                # using the shared whole-record oracle and compare early to final data.
                if kind in probes and arm.startswith('four-step'):
                    early = probes[kind]['response']['answer']
                    if early != row['response']['answer']['report']:
                        raise ValueError('lab whole Get differs from the final public whole Get')
            else:
                validate_read('current', command, row['response'], manifest)
                if kind in probes and arm.startswith('four-step'):
                    early = probes[kind]['response']['answer']['page']
                    final = row['response']['answer']['report']['page']
                    if early['rows'] != final['rows']:
                        raise ValueError('lab Find differs from final public Find')
            probes.setdefault(kind, dict(parent_received_ns=row['parent_received_ns'], worker_ns=None,
                path=f'public Host::{kind} after Ready', response=row['response'],
                parent_requested_ns=row['parent_requested_ns']))
        search = call({'op': 'search', 'query': COMMON}, name='canonical_search')
        validate_read('current', search['command'], search['response'], manifest)
        search['path'] = 'public Host::search after Ready'
        probes.setdefault('search', search)
        linked = call({'op': 'get', 'target': LINK_TARGET, 'whole': True}, name='canonical_get_linked')
        validate_link_get(linked['response'], manifest)
        if arm.startswith('four-step'):
            early_doc = get_targets[LINK_TARGET]['response']['answer']['document']
            final_doc = linked['response']['answer']['report']['document']
            compare_own_structure(early_doc, final_doc)
            for target, probe in get_targets.items():
                probe['requested_document_structure_ns'] = lab['requested_documents_structure'][target]['worker_ns']
                probe['request_ns'] = lab['worker_request_ns'][target]
        get_targets.setdefault('bench/small.md', probes['get'])
        get_targets.setdefault(LINK_TARGET, dict(parent_received_ns=linked['parent_received_ns'],
            worker_ns=None, path='public Host::get after Ready (early attempt unavailable)',
            response=linked['response'], query_host_ns=linked['host_ns']))
        if mid_target:
            mid_final = call({'op': 'get', 'target': mid_target, 'whole': True}, name='canonical_get_mid')
            validate_mid_get(mid_final['response'], manifest)
            get_targets.setdefault(mid_target, dict(parent_received_ns=mid_final['parent_received_ns'],
                worker_ns=None, path='public Host::get after Ready', response=mid_final['response'],
                query_host_ns=mid_final['host_ns']))
            if arm.startswith('four-step'):
                compare_own_structure(get_targets[mid_target]['response']['answer']['document'],
                                      mid_final['response']['answer']['report']['document'])
        canonical = observations.copy()
        state = {name: dict(path=name, body=body, fields={'status': 'draft', 'perf_bucket': 'probe'})
                 for name, body in manifest['probe_bodies'].items()}
        warm = []
        for name, base in READS + [(name, None) for name in WRITES]:
            command = base or mutation(name, 0)
            before = 'bench/subject.md' if command['op'] == 'set' else command.get('from')
            before_bytes = (root / 'vault' / before).read_bytes() if before else None
            row = call(command, name=name)
            if base:
                validate_read('current', command, row['response'], manifest)
            else:
                # Keep verification calls in the replay history, outside operation clocks.
                class RecordingWorker:
                    def call(self, command):
                        read = call(command, name=name + '_readback')
                        return read['response'], read['round_trip_ns']
                verify_mutation(RecordingWorker(), 'current', command, root, row['response'], state, before_bytes)
            row['verified'] = True
            warm.append(row)
        end = endpoint(origin, path='after twelve warm operations and their readbacks',
                       resources=resources(worker.process.pid))
    finally:
        worker.close()
    # Warm writes changed source; audit against the actual end tree while retaining
    # the original frozen fixture identity as the cross-arm gate key.
    from fixture import tree_digest
    final_paths = sorted(p.relative_to(root / 'vault').as_posix() for p in (root / 'vault').rglob('*.md'))
    final_manifest = dict(manifest, tree_sha256=tree_digest(root / 'vault'), paths=final_paths,
                          document_count=len(final_paths),
                          markdown_bytes=sum((root / 'vault' / p).stat().st_size for p in final_paths))
    store = audit_store(binary, root, final_manifest)
    if integrity_binary is not None:
        integrity['post_workload'] = verify_store(integrity_binary, root, 'post_workload')
    if arm == 'health-once':
        health_once_final_gate(validate_receipts, events, worker.ready, manifest, store)
    final_path_ids = identity_map(next((root / 'data').rglob('store.sqlite3')))
    final_layout = store_layout(next((root / 'data').rglob('store.sqlite3')))
    layout = documents_layout(next((root / 'data').rglob('store.sqlite3')))
    write_json(root / 'request-history.json', history)
    write_json(root / 'observations.json', observations)
    responses = [row['response'] for row in observations]
    replay = validate_provenance(root, history, responses, store, workload=cell_workload(mid_target))
    # Bind retained responses to raw native stdout before accepting the replay.
    from transport import PREFIX
    transcript = [json.loads(line.removeprefix(PREFIX)) for line in
                  (root / 'worker.stdout.log').read_text().splitlines() if line.startswith(PREFIX)]
    ready_at = [i for i, row in enumerate(transcript) if row.get('ready') is True]
    if len(ready_at) != 1 or transcript[ready_at[0] + 1:] != responses:
        raise ValueError('retained responses differ from raw native worker stream')
    result = dict(arm=arm, fixture_sha256=manifest['tree_sha256'], document_count=manifest['document_count'],
        origin_monotonic_ns=origin, clock_scope='parent origin before Current construction in every arm; worker clocks separate, never added',
        public_ready=public_ready, ready=public_ready, health_complete=public_ready,
        first_get=describe_probe(probes['get'], arm, 'get'),
        first_find=describe_probe(probes['find'], arm, 'find'),
        first_search=describe_probe(probes['search'], arm, 'search'), public_search_after_ready=search,
        stages=events, worker_ready=worker.ready, warm_operations=warm, canonical_operations=canonical, end=end, store=store,
        post_ready_store=post_ready_store, post_ready_store_path_ids=post_ready_path_ids, store_path_ids=final_path_ids,
        first_get_targets={path: describe_probe(probe, arm, 'get') for path, probe in get_targets.items()},
        layout={'post_ready': post_ready_layout, 'post_workload': final_layout},
        documents_layout=layout, integrity=integrity, replay_verified=replay,
        unavailable={'commits': 'ordinary job evidence exposes changesets, not committed transaction count'},
        resource_scope='Linux main-process RSS sampled on receipt; excludes controller, not exact publication instants')
    if mid_public is not None:
        result['mid_request'] = mid_public
    if arm.startswith('four-step'):
        result.update(lab)
        result['unavailable'].pop('commits')
    elif arm == 'health-once':
        event = events[-1]
        result['ready'] = result['health_complete'] = dict(parent_received_ns=event['parent_received_ns'],
            worker_ns=event['stage_boundary_ns'], resources=event.get('resources_when_observed'),
            source='health-once-health full-build end', counters=event['health_once_work'])
        result['health_once_stored_links_gate'] = dict(stored_links=store['counts']['links'],
            source='post-close audit; validated fixed warm workload preserves link cardinality')
    return result


def health_once_final_gate(validator, events, ready, manifest, store):
    """Use the archived exactly-once check after the link-count-preserving warm suffix.

    The fixed workload changes a scalar field, creates a link-free document, and
    renames paths. verify_mutation checks unchanged moved bytes and every rewritten
    referrer's bytes: each rewrite replaces only one link destination. No operation
    adds or removes links, so the final audited count is also the initial count.
    """
    validator('health-once', events, ready, manifest, stored_links=store['counts']['links'])


def cell_workload(mid_target=None):
    """The existing twelve operations, one sample each, plus authored preparation/readbacks."""
    from baseline import READS, WRITES, mutation
    commands = [{'op': 'get', 'target': 'small', 'whole': True}, {'op': 'find'},
                {'op': 'search', 'query': __import__('fixture').COMMON},
                {'op': 'get', 'target': LINK_TARGET, 'whole': True}]
    if mid_target:
        commands.append({'op': 'get', 'target': mid_target, 'whole': True})
    for name, base in READS + [(name, None) for name in WRITES]:
        command = base or mutation(name, 0)
        commands.append(command)
        if base is None:
            target = {'set': 'bench/subject.md', 'new': command.get('path'), 'move': command.get('to')}[command['op']]
            commands.append({'op': 'get', 'target': target})
    return commands


def check_main_source(repo, base=MAIN_BASE):
    """The main measurement differs only in its performance worker controls."""
    import subprocess
    command = ['git', 'diff', base, '--', ':(glob)crates/*/src/**']
    output = subprocess.check_output(command, cwd=repo, text=True)
    if output:
        raise ValueError('main worker product sources differ from the pinned baseline')
    allowed = lambda name: (name == 'crates/norn-host/tests/performance.rs'
                            or name.startswith('crates/norn-host/tests/performance/') and name.endswith('.rs'))
    changed = subprocess.check_output(['git', 'diff', '--name-only', base], cwd=repo, text=True).splitlines()
    untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard'], cwd=repo, text=True).splitlines()
    if any(not allowed(name) for name in changed + untracked):
        raise ValueError('main worker must preserve pinned product sources and change only performance Rust tests')
    return dict(base=base, product_sources_equal=True, command=command, output=output)


def prepare_builds(root, repo):
    """Build exact detached sources with each revision's existing receipt producer."""
    import shutil
    import subprocess
    import sys
    from provenance import verify_build, sha256
    root.mkdir(parents=True, exist_ok=True)
    sources = root / 'sources'
    sources.mkdir(exist_ok=True)
    config = {'schema_version': 1, 'builds': {}}
    revisions = REVISIONS | {'prototype': subprocess.check_output(
        ['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()}
    for role in ('main', 'prototype', 'health-once'):
        source = sources / role
        if source.exists():
            clean = subprocess.check_output(['git', 'status', '--porcelain'], cwd=source, text=True)
            detached = subprocess.run(['git', 'symbolic-ref', '-q', 'HEAD'], cwd=source, capture_output=True)
            overlay = 'crates/norn-host/tests/performance/public.rs'
            if role == 'main' and clean.strip() == 'M ' + overlay and (source / overlay).read_bytes() == (repo / overlay).read_bytes():
                subprocess.run(['git', 'restore', '--', overlay], cwd=source, check=True)
                clean = ''
            if clean or detached.returncode != 1:
                raise ValueError('reused build sources must be clean detached worktrees (apart from the exact main harness overlay)')
            subprocess.run(['git', 'checkout', '--detach', revisions[role]], cwd=source, check=True)
        else:
            subprocess.run(['git', 'worktree', 'add', '--detach', str(source), revisions[role]], cwd=repo, check=True)
        if role == 'main':
            overlay = Path('crates/norn-host/tests/performance/public.rs')
            shutil.copy2(repo / overlay, source / overlay)
        main_check = check_main_source(source) if role == 'main' else None
        receipt_path = root / f'{role}.build.json'
        # Outside each source checkout, inside the enclosing harness checkout.
        subprocess.run([sys.executable, str(source / 'scripts/performance/provenance.py'),
            '--target-directory', str(root / 'cargo-targets' / role), '--receipt', str(receipt_path),
            '--startup-profile'], cwd=source, check=True)
        receipt = verify_build(receipt_path, source)
        frozen_binary = root / f'{role}.worker'
        shutil.copy2(receipt['executable'], frozen_binary)
        if main_check is not None:
            receipt['main_product_source_check'] = main_check
        receipt['executable'] = str(frozen_binary)
        receipt['sha256'] = sha256(frozen_binary)
        receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
        config['builds'][role] = dict(source=str(source), receipt=str(receipt_path))
    toolchains = {json.loads(Path(item['receipt']).read_text())['rustc'] for item in config['builds'].values()}
    if len(toolchains) != 1:
        raise ValueError('prepare-builds must compile all roles with one toolchain')
    config['rustc'] = toolchains.pop()
    (root / 'config.json').write_text(json.dumps(config, indent=2) + '\n')
    return root / 'config.json'


def check_prototype_source(repo, built_head):
    """Permit Python-only follow-up commits without silently measuring different Rust."""
    import subprocess
    inputs = ['crates', 'vendor', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo']
    changed = subprocess.run(['git', 'diff', '--quiet', built_head, '--', *inputs], cwd=repo)
    untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard',
                                        '--', *inputs], cwd=repo)
    if changed.returncode or untracked:
        raise ValueError('prototype Rust sources differ from the compiled receipt; commit and rebuild')


def checked_build(config, arm):
    from provenance import verify_build
    role = 'prototype' if arm.startswith('four-step') else arm
    item = config['builds'][role]
    source = Path(item['source'])
    receipt = verify_build(Path(item['receipt']), source)
    if 'induced-failure' not in receipt['features']:
        raise ValueError('loading cells require induced-failure worker features')
    import subprocess
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=source, text=True).strip()
    if receipt['repository_head'] != head:
        raise ValueError('source HEAD differs from the build receipt')
    if role in REVISIONS:
        expected = subprocess.check_output(['git', 'rev-parse', REVISIONS[role]], cwd=source, text=True).strip()
        if head != expected:
            raise ValueError('arm source differs from its pinned revision')
    if role == 'main' and receipt.get('main_product_source_check') != check_main_source(source):
        raise ValueError('main worker receipt lacks product source equality evidence')
    if role == 'main':
        overlay = Path('crates/norn-host/tests/performance/public.rs')
        if (source / overlay).read_bytes() != (Path(__file__).resolve().parents[2] / overlay).read_bytes():
            raise ValueError('main worker lacks the current public mid-request harness; prepare builds again')
    if role == 'prototype':
        check_prototype_source(Path(__file__).resolve().parents[2], head)
    return receipt, source


def freeze_fixtures(config, destination, sizes, base=None):
    """Freeze all source trees before the first measured process starts."""
    from types import SimpleNamespace
    from baseline import prepare, write_json
    from fixture import tree_digest
    receipt, source = checked_build(config, 'main')
    # Reuse only base fixtures bound to the unchanged generator and probe authoring.
    if base is not None:
        import subprocess
        changed = subprocess.run(['git', 'diff', '--quiet', MAIN_BASE, '--',
                                  'crates/norn-fixtures', 'scripts/performance/fixture.py'], cwd=Path(__file__).resolve().parents[2])
        if changed.returncode:
            raise ValueError('base fixture generator differs')
    for size in sizes:
        frozen, manifest = prepare(SimpleNamespace(output=destination, fixtures=base,
                                  current=Path(receipt['executable'])), size - 14)
        if manifest.get('matrix_fixture_version') is not None:
            raise ValueError('base fixture is already enriched; freeze only an original base')
        if any(raw in manifest['large_body'] for raw in ('[[small#Details]]', '[Small](small.md#Details)', '[[matrix-missing-target]]')):
            raise ValueError('base fixture already contains injected links')
        if size == 2014 and manifest['tree_sha256'] != BASE_2014:
            raise ValueError('existing n2014 base fixture does not match the known generator hash')
        manifest['base_fixture_sha256'] = manifest['tree_sha256']
        # Keep fourteen probes and the original small document. The second whole
        # target adds both link families, a heading anchor, and a broken target.
        links = "\n[[small#Details]]\n[Small](small.md#Details)\n[[matrix-missing-target]]\n"
        body = manifest['large_body'].replace(manifest['section'], links + manifest['section'])
        manifest['large_body'] = body
        manifest['probe_bodies'][LINK_TARGET] = body
        (frozen / 'vault' / LINK_TARGET).write_text('---\nstatus: draft\nperf_bucket: probe\n---\n' + body)
        manifest.update(matrix_fixture_version=2, link_target=LINK_TARGET,
                        tree_sha256=tree_digest(frozen / 'vault'),
                        markdown_bytes=sum((frozen / 'vault' / name).stat().st_size for name in manifest['paths']))
        write_json(frozen / 'manifest.json', manifest)
    return destination / 'fixtures'


def run_matrix(config, fixtures, output, sizes, arms, repetitions=None, gate_reference=None, *,
               fts_mode='rebuild', mid_request_reference=None, mid_request_offset_ns=None, skip_warmup=False, row_start=0):
    import shutil
    import subprocess
    import sys
    from types import SimpleNamespace
    from baseline import prepare, write_json
    from provenance import source_tree_digest
    if fixtures is None:
        raise ValueError('timed runs require --fixtures; freeze every size before timing')
    cells = selected_cells(sizes, arms, repetitions, skip_warmup, row_start)
    seeds = mid_request_seeds(mid_request_reference, fts_mode) if mid_request_reference else {}
    if mid_request_offset_ns is not None and (type(mid_request_offset_ns) is not int or mid_request_offset_ns < 0 or len(sizes) != 1):
        raise ValueError('a direct mid-request offset requires one size and a nonnegative integer')
    available = set(seeds)
    for cell in cells:
        if cell['discarded_warmup']:
            continue
        if cell['arm'] == 'four-step':
            available.add(cell['documents'])
        if cell['arm'] == 'main' and mid_request_offset_ns is None and cell['documents'] not in available:
            raise ValueError('measured main needs a preceding four-step cell; use --row-start 1, --mid-request-reference, or --mid-request-offset-ns')
    output.mkdir(parents=True)
    snapshot = source_tree_digest(Path(__file__).resolve().parents[2])
    metadata = dict(schema_version=2, fts_mode=fts_mode, row_start=row_start, skipped_warmup=skip_warmup,
                    mid_request_reference=str(mid_request_reference) if mid_request_reference else None, completed=False, cells=cells, config=config,
                    harness_source_sha256=snapshot, results=[], semantic_equal=False,
                    busy_thresholds=BUSY_THRESHOLDS,
                    conditions='serial release workers; frozen sources; Williams rows; explicit warmup selection')
    write_json(output / 'plan.json', dict(serial=True, cells=cells))
    write_json(output / 'run.json', metadata)
    gate_reference = gate_reference or fixtures / 'loading-gate-reference.json'
    references = json.loads(gate_reference.read_text()) if gate_reference.exists() else {}
    completed = []
    try:
        # Validate ALL receipts, toolchains and frozen fixtures before ANY timed cell.
        builds = {arm: checked_build(config, arm) for arm in ARMS}
        if len({receipt['rustc'] for receipt, _ in builds.values()}) != 1:
            raise ValueError('all roles must use one toolchain')
        prepared = {}
        for documents in sizes:
            prepared[documents] = prepare(SimpleNamespace(output=output, fixtures=fixtures,
                current=Path(builds['main'][0]['executable'])), documents - 14)
            manifest = prepared[documents][1]
            if (manifest['document_count'] != documents or manifest.get('matrix_fixture_version') != 2
                    or any(manifest['large_body'].count(raw) != 1 for raw in
                           ('[[small#Details]]', '[Small](small.md#Details)', '[[matrix-missing-target]]'))):
                raise ValueError('fixture must have the requested count and linked-target version 2')
            if documents in seeds and (seeds[documents]['fixture_sha256'] != manifest['tree_sha256']
                                       or seeds[documents]['path'] != mid_request_target(manifest)):
                raise ValueError('mid-request reference frozen fixture or target differs')
        for cell in cells:
            arm, documents = cell['arm'], cell['documents']
            receipt, source = builds[arm]
            binary = Path(receipt['executable'])
            frozen, manifest = prepared[documents]
            label = 'warmup-discarded' if cell['discarded_warmup'] else f"r{cell['repetition']:02}-p{cell['position']}"
            root = (output / f'n{documents}-{label}-{arm}').resolve()
            root.mkdir()
            shutil.copytree(frozen / 'vault', root / 'vault')
            write_json(root / 'manifest.json', manifest)
            offset = (mid_request_offset_ns if mid_request_offset_ns is not None else seeds.get(documents, {}).get('offset_ns')) if arm == 'main' and not cell['discarded_warmup'] else None
            integrity_binary = builds['four-step'][0]['executable'] if arm != 'health-once' else None
            spec = dict(fts_mode=fts_mode, mid_request_offset_ns=offset, integrity_binary=integrity_binary, arm=arm, binary=str(binary), source=str(source), manifest=manifest,
                        root=str(root), build=receipt, harness_source_sha256=snapshot, cell=cell)
            write_json(root / 'cell.json', spec)
            await_idle(root / 'host-admission.json')
            subprocess.run([sys.executable, str(Path(__file__).resolve()), '--cell', str(root / 'cell.json')], check=True)
            result = json.loads((root / 'result.json').read_text())
            metadata['results'].append(str(root / 'result.json'))
            gate_semantics(references, result)
            write_json(gate_reference, references)
            if arm == 'four-step':
                seeds[documents] = dict(path=result['mid_request']['target'], offset_ns=result['mid_request']['request_ns'],
                                        fixture_sha256=result['fixture_sha256'])
            completed.append((cell, result))
            write_json(output / 'summary.json', summarize(completed))
            print(f"{label}: " + cell_summary(documents, result), flush=True)
        for arm in builds:
            checked_build(config, arm)
        if snapshot != source_tree_digest(Path(__file__).resolve().parents[2]):
            raise ValueError('harness source changed during the run')
        metadata['completed'] = True
        metadata['semantic_equal'] = True
        metadata['gate_reference'] = str(gate_reference.resolve())
    except BaseException as error:
        metadata['failure'] = repr(error)
        raise
    finally:
        write_json(output / 'run.json', metadata)


def main():
    import sys
    here = Path(__file__).resolve().parent
    # Public validators use sibling imports, matching the existing controller.
    sys.path.insert(0, str(here / 'live_mode'))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepare-plan', action='store_true')
    parser.add_argument('--prepare-builds', type=Path)
    parser.add_argument('--config', type=Path)
    parser.add_argument('--freeze-fixtures', type=Path, help='prepare linked frozen fixtures outside timing')
    parser.add_argument('--repetitions', type=int, help='Williams rows per size; defaults 2014=8, 10014=4, 314=1')
    parser.add_argument('--fts-mode', choices=('rebuild', 'batched'), default='rebuild', help='lab-only step-5 FTS construction')
    parser.add_argument('--mid-request-reference', type=Path, help='completed four-step run directory supplying per-size main wall offsets')
    parser.add_argument('--mid-request-offset-ns', type=int, help='explicit prior lab worker-clock request offset; one size only')
    parser.add_argument('--row-start', type=int, choices=range(4), default=0, help='zero-based first Williams row; row 1 permits same-run main offset seeding')
    parser.add_argument('--skip-warmup', action='store_true', help='omit a warmup already completed in this serial measurement session')
    parser.add_argument('--gate-reference', type=Path, help='persistent cross-run semantic and path-id reference')
    parser.add_argument('--fixtures', type=Path, help='existing frozen fixture parent, keyed by background count')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--sizes', type=int, nargs='+', default=[2014, 10014], help='total documents, including fourteen probes')
    parser.add_argument('--arms', choices=ARMS, nargs='+', default=list(ARMS))
    parser.add_argument('--cell', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.cell:
        spec = json.loads(args.cell.read_text())
        if spec['arm'] == 'health-once':
            sys.path.insert(0, str(Path(spec['source']) / 'scripts/performance'))
        result = measure_cell(Path(spec['binary']), Path(spec['root']), spec['manifest'], spec['arm'],
                              fts_mode=spec.get('fts_mode', 'rebuild'), mid_request_offset_ns=spec.get('mid_request_offset_ns'),
                              integrity_binary=Path(spec['integrity_binary']) if spec.get('integrity_binary') else None)
        result['build'] = spec['build']
        result['harness_source_sha256'] = spec['harness_source_sha256']
        (Path(spec['root']) / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    elif args.prepare_plan:
        print(json.dumps(dict(serial=True, cells=selected_cells(args.sizes, args.arms, args.repetitions, args.skip_warmup, args.row_start)), indent=2))
    elif args.freeze_fixtures:
        if args.config is None:
            parser.error('--freeze-fixtures requires --config')
        print(freeze_fixtures(json.loads(args.config.read_text()), args.freeze_fixtures.resolve(), args.sizes, args.fixtures))
    elif args.prepare_builds:
        print(prepare_builds(args.prepare_builds.resolve(), here.parents[1]))
    else:
        if args.config is None or args.output is None:
            parser.error('provide --config and --output, or use --prepare-plan / --prepare-builds')
        if len(set(args.arms)) != len(args.arms):
            parser.error('arms must be unique')
        run_matrix(json.loads(args.config.read_text()), args.fixtures, args.output.resolve(), args.sizes, args.arms, args.repetitions, args.gate_reference,
                   fts_mode=args.fts_mode, mid_request_reference=args.mid_request_reference,
                   mid_request_offset_ns=args.mid_request_offset_ns, skip_warmup=args.skip_warmup, row_start=args.row_start)


if __name__ == '__main__':
    main()
