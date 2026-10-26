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
        if stage.get('path') not in ('bench/small.md', LINK_TARGET):
            raise ValueError('lab hook answered the wrong target')
        if stage['path'] == LINK_TARGET:
            wrapped.update(projection_epoch=stage['snapshot']['epoch'],
                           projection_generation=stage['snapshot']['generation'])
            validate_link_get(wrapped, manifest, health_loading=True)
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


def validate_link_get(response, manifest, health_loading=False, *, first_index=False):
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
    if doc['links']['items'] != expected_links:
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
    result = dict(probe, timing_scope=('estimated earliest availability plus builder query time' if lab else
                                     'first validated public answer; includes admission and builder query time'),
                  delivery_scope=('buffered in worker; serialized and delivered after build' if lab else
                                  'public response delivered when answered'))
    if lab:
        available, started, finished, duration, estimate = (probe[key] for key in (
            'earliest_available_ns', 'query_started_ns', 'query_finished_ns', 'query_host_ns', 'earliest_answer_ns'))
        if (any(type(value) is not int for value in (available, started, finished, duration, estimate))
                or not 0 <= available <= started <= finished or duration != finished - started
                or estimate != available + duration):
            raise ValueError('invalid lab query timing')
        result.update(worker_ns=estimate,
                      worker_clock_scope='derived earliest-answer estimate; not actual finish',
                      availability_clock_scope='prerequisite completion observed on worker clock')
    if kind == 'get':
        result['findings_scope'] = ('answered before link health (health loading)' if lab else
                                    'includes link-health findings')
    return result


def lab_measurements(arm, events):
    names = [e['startup_stage'].removeprefix('four-step-') for e in events]
    # Preserve exact pass/probe order while allowing one structure/Get pair per target.
    expected = ['names', 'content', 'find']
    if arm == 'four-step':
        expected.append('search')
    expected.append('structure')
    if arm == 'four-step-no-body':
        expected.append('search')
    expected.append('relations')
    if [name for name in names if name not in ('requested-structure', 'get')] != expected:
        raise ValueError('four-step boundary/probe coverage differs')
    requested = [e for e in events if e['startup_stage'] == 'four-step-requested-structure']
    gets = [e for e in events if e['startup_stage'] == 'four-step-get']
    if not requested or len(requested) != len(gets):
        raise ValueError('four-step requested target coverage differs')
    stages = {e['startup_stage']: e for e in events}
    report = stages['four-step-relations']
    if report['reconciled'] is not False:
        raise ValueError('source churn required an ordinary heal during the lab build')
    steps = ('names', 'content', 'structure', 'relations')
    boundaries = report['boundary_ns']
    busy = report['busy_ns']
    if set(boundaries) != set(steps) or set(busy) != set(steps):
        raise ValueError('four-step timing coverage differs')
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
    result = dict(step_counters=report['steps'], step_boundary_ns=boundaries,
                  step_boundary_intervals_ns=intervals, step_busy_ns=busy, pull_ahead_ns=pull_ahead,
                  step_boundary_scope='lab build origin; end of each main pass, before its observer queries; relations includes four_step::finish teardown and authority check',
                  step_busy_scope='main-pass elapsed work excluding observer queries and requested-path pull-ahead; relations includes four_step::finish teardown and authority check',
                  pull_ahead_scope='requested-path content and structure elapsed work; excludes observer queries',
                  step_duration_scope='boundary intervals include intervening observers and pull-ahead; busy_ns is elapsed work, not CPU time',
                  counter_scope={
                      'files_read': 'source-read attempts, including failed opens and paths refused before opening',
                      'rows_rewritten': 'content/metadata transitions; separate no-body payload updates are body_rows_rewritten',
                      'fts': 'SQLite internal FTS trigger and shadow-table work is uncounted',
                      'findings_discard': 'skipping findings discard on a fresh store is available to ordinary loading too; not a four-step-specific saving'},
                  content_complete=stages['four-step-content'])
    for field, name in [('ready', 'structure'), ('health_complete', 'relations'),
                        ('requested_document_structure', 'requested-structure')]:
        event = stages['four-step-' + name]
        result[field] = dict(parent_received_ns=event['parent_received_ns'], worker_ns=event['elapsed_ns'],
                             resources=event.get('resources_when_observed'), source=event['startup_stage'],
                             delivery_scope='buffered in worker; delivered after build',
                             resource_scope='sampled after build on buffered event receipt')
    result['requested_documents_structure'] = {e.get('path', 'bench/small.md'): dict(worker_ns=e['elapsed_ns'], parent_received_ns=e['parent_received_ns']) for e in requested}
    result['ready']['counters'] = {step: result['step_counters'][step] for step in steps[:3]}
    result['health_complete']['counters'] = result['step_counters']
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
        if 'earliest_answer_ns' in probe:
            clock = (f"estimated_earliest_answer {worker / 1e9:.6f}s "
                     f"actual_query_finished {probe['query_finished_ns'] / 1e9:.6f}s ")
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
    lines.append(f"  semantic {result['store']['semantic_sha256']}")
    return '\n'.join(lines)


def measure_cell(binary, root, manifest, arm):
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
    origin = time.perf_counter_ns()

    def on_stage(stage):
        stage['parent_received_ns'] = time.perf_counter_ns() - origin
        name = stage['startup_stage']
        if arm == 'main':
            from public_mode import validate_attempt
            index = len(events)
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
                    earliest_answer_ns=stage['earliest_answer_ns'])
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
    if arm.startswith('four-step'):
        options['loading_options'] = {'transaction_bound': 128, 'requested_paths': ['bench/small.md', LINK_TARGET]}
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
        elif arm == 'health-once':
            validate_receipts(arm, events, worker.ready, manifest)
        elif (len(events) != len(PUBLIC_COMMANDS)
              or worker.ready['operator_prefix_receipt']['logical_requests'] != len(PUBLIC_COMMANDS)):
            raise ValueError('main must execute all three public prefix attempts')

        # Untimed independent gate while the measured process is idle, before any
        # query or rename can conceal a step-4 health difference.
        post_ready_store = audit_store(binary, root, manifest)
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
            for key in ('path', 'body', 'fields', 'headings', 'blocks', 'tags', 'links'):
                if early_doc[key] != final_doc[key]:
                    raise ValueError('linked lab Get exact content/structure differs from public Get')
            for target, probe in get_targets.items():
                probe['requested_document_structure_ns'] = lab['requested_documents_structure'][target]['worker_ns']
        get_targets.setdefault('bench/small.md', probes['get'])
        get_targets.setdefault(LINK_TARGET, dict(parent_received_ns=linked['parent_received_ns'],
            worker_ns=None, path='public Host::get after Ready (early attempt unavailable)',
            response=linked['response'], query_host_ns=linked['host_ns']))
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
    if arm == 'health-once':
        health_once_final_gate(validate_receipts, events, worker.ready, manifest, store)
    final_path_ids = identity_map(next((root / 'data').rglob('store.sqlite3')))
    final_layout = store_layout(next((root / 'data').rglob('store.sqlite3')))
    layout = documents_layout(next((root / 'data').rglob('store.sqlite3')))
    write_json(root / 'request-history.json', history)
    write_json(root / 'observations.json', observations)
    responses = [row['response'] for row in observations]
    replay = validate_provenance(root, history, responses, store, workload=cell_workload())
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
        documents_layout=layout, replay_verified=replay,
        unavailable={'commits': 'ordinary job evidence exposes changesets, not committed transaction count'},
        resource_scope='Linux main-process RSS sampled on receipt; excludes controller, not exact publication instants')
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


def cell_workload():
    """The existing twelve operations, one sample each, plus authored preparation/readbacks."""
    from baseline import READS, WRITES, mutation
    commands = [{'op': 'get', 'target': 'small', 'whole': True}, {'op': 'find'},
                {'op': 'search', 'query': __import__('fixture').COMMON},
                {'op': 'get', 'target': LINK_TARGET, 'whole': True}]
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
            if clean or detached.returncode != 1:
                raise ValueError('reused build sources must be clean detached worktrees')
            subprocess.run(['git', 'checkout', '--detach', revisions[role]], cwd=source, check=True)
        else:
            subprocess.run(['git', 'worktree', 'add', '--detach', str(source), revisions[role]], cwd=repo, check=True)
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


def run_matrix(config, fixtures, output, sizes, arms, repetitions=None, gate_reference=None):
    import shutil
    import subprocess
    import sys
    from types import SimpleNamespace
    from baseline import prepare, write_json
    from provenance import source_tree_digest
    if fixtures is None:
        raise ValueError('timed runs require --fixtures; freeze every size before timing')
    cells = [c for c in plan(sizes, repetitions) if c['arm'] in arms or c['discarded_warmup']]
    output.mkdir(parents=True)
    snapshot = source_tree_digest(Path(__file__).resolve().parents[2])
    metadata = dict(schema_version=2, completed=False, cells=cells, config=config,
                    harness_source_sha256=snapshot, results=[], semantic_equal=False,
                    busy_thresholds=BUSY_THRESHOLDS,
                    conditions='serial release workers; frozen sources; Williams rows; one discarded warmup per size')
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
            spec = dict(arm=arm, binary=str(binary), source=str(source), manifest=manifest,
                        root=str(root), build=receipt, harness_source_sha256=snapshot, cell=cell)
            write_json(root / 'cell.json', spec)
            await_idle(root / 'host-admission.json')
            subprocess.run([sys.executable, str(Path(__file__).resolve()), '--cell', str(root / 'cell.json')], check=True)
            result = json.loads((root / 'result.json').read_text())
            metadata['results'].append(str(root / 'result.json'))
            gate_semantics(references, result)
            write_json(gate_reference, references)
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
        result = measure_cell(Path(spec['binary']), Path(spec['root']), spec['manifest'], spec['arm'])
        result['build'] = spec['build']
        result['harness_source_sha256'] = spec['harness_source_sha256']
        (Path(spec['root']) / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    elif args.prepare_plan:
        print(json.dumps(dict(serial=True, cells=[c for c in plan(args.sizes, args.repetitions) if c['arm'] in args.arms or c['discarded_warmup']]), indent=2))
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
        run_matrix(json.loads(args.config.read_text()), args.fixtures, args.output.resolve(), args.sizes, args.arms, args.repetitions, args.gate_reference)


if __name__ == '__main__':
    main()
