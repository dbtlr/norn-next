"""Experimental design, persisted gates, and descriptive loading summaries."""
import json
import os
import sqlite3
import statistics
import time
from contextlib import closing
from pathlib import Path

ARMS = ('main', 'four-step', 'four-step-no-body', 'health-once')
WILLIAMS = (
    ('main', 'four-step', 'health-once', 'four-step-no-body'),
    ('four-step', 'four-step-no-body', 'main', 'health-once'),
    ('four-step-no-body', 'health-once', 'four-step', 'main'),
    ('health-once', 'main', 'four-step-no-body', 'four-step'),
)
LINK_TARGET = 'bench/large.md'
BASE_2014 = '3c0fa350593b337a953d14c59dcb240ea5afca93d36d120d6c7b38fc5fd3635a'


def plan(sizes, repetitions=None):
    if not sizes or len(set(sizes)) != len(sizes) or any(n not in (314, 2014, 10014) for n in sizes):
        raise ValueError('choose unique total document counts from 314, 2014, 10014')
    if repetitions is not None and repetitions < 1:
        raise ValueError('repetitions must be positive')
    cells = []
    for size in sizes:
        count = repetitions if repetitions is not None else {314: 1, 2014: 8, 10014: 4}[size]
        orders = [(0, ['main'])] + [(r + 1, WILLIAMS[r % 4]) for r in range(count)]
        for repetition, order in orders:
            for position, arm in enumerate(order):
                cells.append(dict(documents=size, background_documents=size - 14, arm=arm,
                                  repetition=repetition, position=position + 1,
                                  discarded_warmup=repetition == 0, profile='release',
                                  changeset_size=128, transaction_bound=128))
    return cells


def identity_map(path):
    """Read the exact path-to-id pairs, including non-document vault files."""
    with closing(sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True)) as db:
        return {table: db.execute(f'SELECT path,id FROM {table} ORDER BY path').fetchall()
                for table in ('documents', 'vault_files')}


def gate_semantics(references, result):
    """Gate initial and final semantics AND identity against a frozen-fixture reference."""
    # Normalize tuples on first use so persisted JSON references compare identically.
    value = {phase: {'semantic_sha256': result[phase]['semantic_sha256'],
                     'path_ids': result[phase + '_path_ids']}
             for phase in ('post_ready_store', 'store')}
    value = json.loads(json.dumps(value))
    key = result['fixture_sha256']
    if key in references and references[key] != value:
        raise ValueError('semantic_sha256 or path→id equality differs for the same frozen fixture (post-Ready or final)')
    references[key] = value


def store_layout(path):
    """All SQLite B-trees and their overflow pages, inspected without writing."""
    with closing(sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True)) as db:
        rows = db.execute('SELECT name,count(*),sum(payload),sum(unused),'
                          "sum(pagetype='overflow'),sum(pgsize) FROM dbstat GROUP BY name ORDER BY name").fetchall()
        return dict(objects={name: dict(pages=pages, payload_bytes=payload, unused_bytes=unused,
                                        overflow_pages=overflow, page_bytes=page_bytes)
                             for name, pages, payload, unused, overflow, page_bytes in rows},
                    freelist_count=db.execute('PRAGMA freelist_count').fetchone()[0],
                    page_count=db.execute('PRAGMA page_count').fetchone()[0],
                    page_size=db.execute('PRAGMA page_size').fetchone()[0],
                    file_size_bytes=path.stat().st_size,
                    wal_size_bytes=Path(str(path) + '-wal').stat().st_size if Path(str(path) + '-wal').exists() else 0)


def host_sample():
    pressure = {}
    for kind in ('cpu', 'io', 'memory'):
        raw = Path('/proc/pressure', kind).read_text()
        pressure[kind] = {line.split()[0]: {key: float(value) for key, value in
                                          (field.split('=') for field in line.split()[1:])}
                          for line in raw.splitlines()}
    cpus = len(os.sched_getaffinity(0))
    # Respect the cgroup quota when the host exposes more logical CPUs than the job owns.
    quota = Path('/sys/fs/cgroup/cpu.max')
    if quota.exists():
        limit, period = quota.read_text().split()
        if limit != 'max':
            cpus = min(cpus, int(limit) / int(period))
    return dict(monotonic_ns=time.monotonic_ns(), loadavg=Path('/proc/loadavg').read_text().strip(),
                effective_cpus=cpus, pressure=pressure)


BUSY_THRESHOLDS = dict(load1_per_cpu=0.5, cpu_some_avg10=10.0, io_some_avg10=5.0,
                       memory_some_avg10=1.0, max_wait_seconds=300, poll_seconds=5)


def busy_reasons(sample, thresholds=BUSY_THRESHOLDS):
    reasons = []
    if float(sample['loadavg'].split()[0]) > sample['effective_cpus'] * thresholds['load1_per_cpu']:
        reasons.append('load1_per_cpu')
    for kind in ('cpu', 'io', 'memory'):
        key = kind + '_some_avg10'
        if sample['pressure'][kind]['some']['avg10'] > thresholds[key]:
            reasons.append(key)
    return reasons


def await_idle(record, thresholds=BUSY_THRESHOLDS):
    """Sync once per cell; retain every bounded host-admission observation."""
    os.sync()
    began = time.monotonic()
    attempts = []
    while True:
        sample = host_sample()
        sample['busy_reasons'] = busy_reasons(sample, thresholds)
        attempts.append(sample)
        elapsed = time.monotonic() - began
        record.write_text(json.dumps(dict(thresholds=thresholds, waited_seconds=elapsed,
                                          accepted=not sample['busy_reasons'], samples=attempts), indent=2) + '\n')
        if not sample['busy_reasons']:
            return
        if elapsed >= thresholds['max_wait_seconds']:
            raise RuntimeError(f'host busy for bounded wait; refused timed cell: {record}')
        time.sleep(min(thresholds['poll_seconds'], thresholds['max_wait_seconds'] - elapsed))


def distribution(values):
    return dict(samples=len(values), median=statistics.median(values), min=min(values), max=max(values))


def numeric_metrics(value, prefix=''):
    if isinstance(value, dict):
        for key, item in value.items():
            yield from numeric_metrics(item, f'{prefix}.{key}' if prefix else key)
    elif type(value) in (int, float):
        yield prefix, value


def summarize(results):
    """Descriptive repetitions only. Never divide incomparable endpoints."""
    groups = {}
    for cell, result in results:
        if cell['discarded_warmup']:
            continue
        evidence = result['public_ready']['counters']
        counters = {key: evidence[key] for key in ('document_opens', 'documents_derived', 'link_health_keys_resolved',
                    'link_health_candidates_read', 'findings_written')}
        counters['links_judged'] = evidence['links_redecided']
        comparable = dict(public_ready_worker_ns=result['public_ready']['worker_ns'],
                          peak_rss_bytes=result['end']['resources']['VmHWM_bytes'],
                          end_cpu_seconds=result['end']['resources']['cpu_seconds'],
                          work=counters,
                          warm_operations={row['name']: {key: row[key] for key in ('host_ns', 'round_trip_ns')}
                                           for row in result['warm_operations']},
                          canonical={row['name']: {key: row[key] for key in ('host_ns', 'round_trip_ns')}
                                     for row in result['canonical_operations']},
                          layout=result['layout'], gates={'passed': 1})
        descriptive = {key: result[key] for key in ('ready', 'first_get', 'first_find', 'first_search', 'first_get_targets')}
        descriptive.update({key: result[key] for key in ('step_boundary_ns', 'step_busy_ns', 'pull_ahead_ns',
                            'step_counters', 'requested_documents_structure') if key in result})
        descriptive['parent_clocks'] = {'public_ready': result['public_ready']['parent_received_ns']}
        descriptive['lab_resources'] = result.get('ready', {}).get('resources') or {}
        digests = {phase: {key: value for key, value in result[phase].items()
                                          if 'sha256' in key and key != 'semantic_sha256'}
                                   for phase in ('post_ready_store', 'store')}
        # A probe response is evidence, not a timing metric. Avoid recursively reporting
        # document positions, counts, and snapshot ids as if they were measurements.
        def timings(probe):
            return {key: value for key, value in probe.items()
                    if key.endswith('_ns') and type(value) is int}
        for key in ('first_get', 'first_find', 'first_search'):
            descriptive[key] = timings(result[key])
        descriptive['first_get_targets'] = {key: timings(value) for key, value in result['first_get_targets'].items()}
        descriptive['ready'] = timings(result['ready'])
        bucket = groups.setdefault(str(cell['documents']), {}).setdefault(cell['arm'], {'comparable': {}, 'not comparable (side by side, no ratios)': {}})
        if cell['arm'] == 'health-once':
            descriptive['health-once context only'] = comparable
            comparable = {}
        bucket.setdefault('identity/page-map/construction digests (side by side)', []).append(digests)
        for category, values in [('comparable', comparable), ('not comparable (side by side, no ratios)', descriptive)]:
            for metric, value in numeric_metrics(values):
                bucket[category].setdefault(metric, []).append(value)
    for arms in groups.values():
        for categories in arms.values():
            for category, metrics in list(categories.items()):
                if not isinstance(metrics, dict):
                    continue
                categories[category] = {key: distribution(values) for key, values in metrics.items()}
    return dict(statistics='median and min–max; descriptive; no significance tests; discarded warmups excluded',
                comparable='main vs four-step vs four-step-no-body; health-once context only',
                **{'not comparable (side by side, no ratios)': 'lab/public first answers, end-step-3 Ready, parent clocks, lab resources, steps, digests, health-once timings'},
                sizes=groups)
