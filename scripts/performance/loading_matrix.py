#!/usr/bin/env python3
"""Serial, one-process loading cells using the existing performance adapters."""
import argparse
import json
import sqlite3
from contextlib import closing
from pathlib import Path

ARMS = ('main', 'four-step', 'four-step-no-body', 'health-once')
REVISIONS = {'main': '2e213c09', 'health-once': 'e944a338'}


def plan(sizes):
    if not sizes or len(set(sizes)) != len(sizes) or any(n not in (314, 2014, 10014) for n in sizes):
        raise ValueError('choose unique total document counts from 314, 2014, 10014')
    return [dict(documents=n, background_documents=n - 14, arm=arm,
                 profile='release', changeset_size=128, transaction_bound=128)
            for n in sizes for arm in ARMS]


def gate_semantics(references, result):
    digest = result['store']['semantic_sha256']
    if references.setdefault(result['fixture_sha256'], digest) != digest:
        raise ValueError('semantic_sha256 differs across arms for the same frozen fixture')


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
                             'scope': 'documents table only; includes interior, leaf and overflow pages'})


def validate_lab_stage(stage, manifest):
    """Validate the real hook answer against the existing frozen fixture oracle."""
    from results import validate_read
    name = stage['startup_stage']
    if name not in ('four-step-get', 'four-step-find'):
        return
    if stage.get('current') is not True:
        raise ValueError('lab hook lost its current source')
    report = stage['answer']
    wrapped = {'answer': {'report': report, 'unsatisfied': stage['unsatisfied'],
                           'advisories': stage.get('advisories', [])}}
    if name == 'four-step-get':
        if stage.get('path') != 'bench/small.md':
            raise ValueError('lab hook answered the wrong target')
        validate_read('current', {'op': 'get', 'target': 'bench/small.md'}, wrapped, manifest)
        document = report['document']
        for key in ('headings', 'blocks', 'tags', 'links', 'findings'):
            collection = document[key]
            if collection['total'] != len(collection['items']):
                raise ValueError('lab whole Get truncated a collection')
    else:
        validate_read('current', {'op': 'find'}, wrapped, manifest)
        page = report['page']
        if (page['moved'] or (page['next'] is not None) != (len(manifest['active_paths']) > 25)
                or any(set(row) != {'path'} for row in page['rows'])):
            raise ValueError('lab metadata page differs from the public path-only page')


def endpoint(origin, *, path, response=None, worker_ns=None, **fields):
    import time
    return dict(parent_received_ns=time.perf_counter_ns() - origin, worker_ns=worker_ns,
                path=path, response=response, **fields)


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

    events, probes = [], {}
    origin = time.perf_counter_ns()

    def on_stage(stage):
        stage['parent_received_ns'] = time.perf_counter_ns() - origin
        name = stage['startup_stage']
        if arm == 'main':
            from public_mode import validate_attempt
            index = stage['operator_prefix_sequence'] - 1
            validate_attempt(index, stage)
            if stage['disposition'] == 'served':
                if index == 0:
                    validate_first_get_content(stage, manifest, first_index=True)
                else:
                    validate_read('current', {'op': 'find'}, stage, manifest)
                probes[('get', 'find')[index]] = dict(
                    parent_received_ns=stage['parent_received_ns'], worker_ns=stage['query_finished_ns'],
                    path=('public Host::get', 'public Host::find')[index], response=stage)
            ack = root / 'operator-prefix-acks' / f'{index + 1}.pending'
            write_json(ack, dict(sequence=index + 1, probe=stage['probe']))
            ack.replace(ack.with_suffix('.json'))
        elif arm.startswith('four-step'):
            validate_lab_stage(stage, manifest)
            if name in ('four-step-get', 'four-step-find'):
                kind = name.removeprefix('four-step-')
                if kind in probes:
                    raise ValueError('duplicate lab probe')
                probes[kind] = dict(parent_received_ns=stage['parent_received_ns'],
                    worker_ns=stage['query_finished_ns'], path=f'lab FourStepRead::{kind}', response=stage)
        events.append(stage)

    if arm == 'main':
        (root / 'operator-prefix-acks').mkdir()
    loading = {'main': 'operator-prefix-public'}.get(arm, arm)
    # Explicit lab controls are supplied by the child process environment. Current
    # strips inherited NORN_* values, so the adapter's typed options own selection.
    options = dict(loading=loading, stage_handler=on_stage)
    if arm.startswith('four-step'):
        options['loading_options'] = {'transaction_bound': 128, 'requested_paths': ['bench/small.md']}
    worker = Current(binary, root, **options)
    history, observations = [], []
    try:
        public_ready = endpoint(origin, path='public Ready control record',
            worker_ns=worker.ready['observed_ready_ns'], counters=worker.ready['job_evidence'],
            resources=resources(worker.process.pid))
        if arm.startswith('four-step'):
            if worker.ready.get('loading_mode') != arm:
                raise ValueError('worker did not execute requested four-step mode')
            expected = ['four-step-names', 'four-step-requested-structure', 'four-step-content',
                        'four-step-find', 'four-step-get', 'four-step-structure', 'four-step-relations']
            if [e['startup_stage'] for e in events] != expected:
                raise ValueError('four-step boundary/probe coverage differs')
        elif arm == 'health-once':
            validate_receipts(arm, events, worker.ready, manifest)
        elif len(events) != 2:
            raise ValueError('main must execute its two public prefix attempts')

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
        search = call({'op': 'search', 'query': COMMON}, name='first_search')
        validate_read('current', search['command'], search['response'], manifest)
        search['path'] = 'public Host::search after Ready'
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
        # The receipt's exactly-once check needs the pre-mutation link count;
        # semantic audit after rename may have changed it, so initial receipts are
        # validated before warm writes, above, without inventing an initial count.
        validate_receipts(arm, events, worker.ready, manifest)
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
        first_get=probes['get'], first_find=probes['find'], first_search=search,
        stages=events, worker_ready=worker.ready, warm_operations=warm, end=end, store=store,
        documents_layout=layout, replay_verified=replay,
        unavailable={'commits': 'ordinary job evidence exposes changesets, not committed transaction count'},
        resource_scope='Linux main-process RSS sampled on receipt; excludes controller, not exact publication instants')
    if arm.startswith('four-step'):
        stages = {e['startup_stage']: e for e in events}
        for field, name in [('ready', 'structure'), ('health_complete', 'relations'),
                            ('requested_document_structure', 'requested-structure')]:
            event = stages['four-step-' + name]
            result[field] = dict(parent_received_ns=event['parent_received_ns'], worker_ns=event['elapsed_ns'],
                resources=event.get('resources_when_observed'), source=event['startup_stage'])
        result['step_counters'] = stages['four-step-relations']['steps']
        result['step_boundary_intervals_ns'] = {}
        result['step_durations_ns'] = None
        previous = 0
        for name in ('names', 'content', 'structure', 'relations'):
            boundary = stages['four-step-' + name]['elapsed_ns']
            result['step_boundary_intervals_ns'][name] = boundary - previous
            previous = boundary
        result['step_duration_scope'] = 'worker boundary intervals; names includes host setup; later intervals include observer probes and serialization'
        result['content_complete'] = stages['four-step-content']
        result['unavailable']['pure_step_durations'] = ('Existing observer clocks only completed boundaries; '
            'requested-document content/structure run before the global content pass. Intervals include probes and cannot isolate step CPU or elapsed work.')
        result['unavailable'].pop('commits')
        result['unavailable']['step_2_search_answer'] = ('FourStepRead exposes get/find only; text Find is rejected; '
            'no allowed worker-only route to lexical search at this boundary. Actual public search recorded after health-complete.')
    elif arm == 'health-once':
        event = events[-1]
        result['ready'] = result['health_complete'] = dict(parent_received_ns=event['parent_received_ns'],
            worker_ns=event['stage_boundary_ns'], resources=event.get('resources_when_observed'),
            source='health-once-health full-build end', counters=event['health_once_work'])
    return result


def cell_workload():
    """The existing twelve operations, one sample each, plus authored preparation/readbacks."""
    from baseline import READS, WRITES, mutation
    commands = [{'op': 'get', 'target': 'small', 'whole': True}, {'op': 'find'},
                {'op': 'search', 'query': __import__('fixture').COMMON}]
    for name, base in READS + [(name, None) for name in WRITES]:
        command = base or mutation(name, 0)
        commands.append(command)
        if base is None:
            target = {'set': 'bench/subject.md', 'new': command.get('path'), 'move': command.get('to')}[command['op']]
            commands.append({'op': 'get', 'target': target})
    return commands


def prepare_builds(root, repo):
    """Build exact detached sources with each revision's existing receipt producer."""
    import shutil
    import subprocess
    import sys
    from provenance import verify_build, sha256
    root.mkdir(parents=True)
    sources = root / 'sources'
    sources.mkdir()
    config = {'schema_version': 1, 'builds': {}}
    revisions = REVISIONS | {'prototype': subprocess.check_output(
        ['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()}
    for role in ('main', 'prototype', 'health-once'):
        source = sources / role
        subprocess.run(['git', 'worktree', 'add', '--detach', str(source), revisions[role]], cwd=repo, check=True)
        receipt_path = root / f'{role}.build.json'
        # Outside each source checkout, inside the enclosing harness checkout.
        subprocess.run([sys.executable, str(source / 'scripts/performance/provenance.py'),
            '--target-directory', str(root / 'cargo-target'), '--receipt', str(receipt_path),
            '--startup-profile'], cwd=source, check=True)
        receipt = verify_build(receipt_path, source)
        frozen_binary = root / f'{role}.worker'
        shutil.copy2(receipt['executable'], frozen_binary)
        receipt['executable'] = str(frozen_binary)
        receipt['sha256'] = sha256(frozen_binary)
        receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
        config['builds'][role] = dict(source=str(source), receipt=str(receipt_path))
    (root / 'config.json').write_text(json.dumps(config, indent=2) + '\n')
    return root / 'config.json'


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
    if role in REVISIONS and not head.startswith(REVISIONS[role]):
        raise ValueError('arm source differs from its pinned revision')
    return receipt, source


def run_matrix(config, fixtures, output, sizes, arms):
    import shutil
    import subprocess
    import sys
    from types import SimpleNamespace
    from baseline import prepare, write_json
    from provenance import source_tree_digest
    cells = [c for c in plan(sizes) if c['arm'] in arms]
    output.mkdir(parents=True)
    snapshot = source_tree_digest(Path(__file__).resolve().parents[2])
    metadata = dict(schema_version=1, completed=False, cells=cells, config=config,
                    harness_source_sha256=snapshot, results=[], semantic_equal=False,
                    conditions='serial release workers; filesystem caches uncontrolled; one sample per warm operation')
    write_json(output / 'run.json', metadata)
    references = {}
    try:
        prepared = {}
        for cell in cells:
            arm, documents = cell['arm'], cell['documents']
            receipt, source = checked_build(config, arm)
            binary = Path(receipt['executable'])
            if documents not in prepared:
                prepared[documents] = prepare(SimpleNamespace(output=output,
                    fixtures=fixtures, current=binary), documents - 14)
            frozen, manifest = prepared[documents]
            if manifest['document_count'] != documents:
                raise ValueError('fixture has the wrong total document count')
            root = (output / f'n{documents}-{arm}').resolve()
            root.mkdir()
            shutil.copytree(frozen / 'vault', root / 'vault')
            write_json(root / 'manifest.json', manifest)
            spec = dict(arm=arm, binary=str(binary), source=str(source), manifest=manifest,
                        root=str(root), build=receipt, harness_source_sha256=snapshot)
            write_json(root / 'cell.json', spec)
            # Each cell controller gets a clean module namespace. Health-once loads
            # its revision's transport and loading receipt validator, never ports them.
            subprocess.run([sys.executable, str(Path(__file__).resolve()), '--cell', str(root / 'cell.json')], check=True)
            result = json.loads((root / 'result.json').read_text())
            metadata['results'].append(str(root / 'result.json'))
            gate_semantics(references, result)
            print(f"{documents} {arm}: Ready {result['ready']['parent_received_ns'] / 1e9:.6f}s "
                  f"Get {result['first_get']['parent_received_ns'] / 1e9:.6f}s "
                  f"Find {result['first_find']['parent_received_ns'] / 1e9:.6f}s "
                  f"semantic {result['store']['semantic_sha256']}", flush=True)
        for arm in arms:
            checked_build(config, arm)
        if snapshot != source_tree_digest(Path(__file__).resolve().parents[2]):
            raise ValueError('harness source changed during the run')
        metadata['completed'] = True
        metadata['semantic_equal'] = len(arms) == len(ARMS)
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
        print(json.dumps(dict(serial=True, cells=[c for c in plan(args.sizes) if c['arm'] in args.arms]), indent=2))
    elif args.prepare_builds:
        print(prepare_builds(args.prepare_builds.resolve(), here.parents[1]))
    else:
        if args.config is None or args.output is None:
            parser.error('provide --config and --output, or use --prepare-plan / --prepare-builds')
        if len(set(args.arms)) != len(args.arms):
            parser.error('arms must be unique')
        run_matrix(json.loads(args.config.read_text()), args.fixtures, args.output.resolve(), args.sizes, args.arms)


if __name__ == '__main__':
    main()
