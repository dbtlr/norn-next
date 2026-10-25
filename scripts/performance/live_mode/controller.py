#!/usr/bin/env python3
"""Private live adapter for the fixed worker workload; one declared cell per invocation."""
import argparse
import copy
import json
import os
from pathlib import Path
import platform
import pwd
import shutil
import sys
import time
from types import SimpleNamespace

sys.dont_write_bytecode=True
from revalidation import MODE, COMMANDS, SCOPE


def read(path):return json.loads(path.read_text())


def configure(config):
    """Use existing runner/oracle modules; only live envelopes have owned adaptations."""
    here=Path(__file__).resolve().parent
    sys.path[:0]=[str(here),config['performance_tools'],config['existing_bundle'],
                  str(Path(config['existing_bundle'])/'runner'),config['prototype_tools']]
    # The existing capped module inserts its frozen runner into sys.path. Bind
    # these two small, compatible content/selection extensions before that import.
    import results, loading
    if not hasattr(results,'validate_first_get_content') or loading.expected_checkpoints('operator-prefix-live')!={('live-get','whole_slug'),('live-find','active_page')}:
        raise ValueError('live content/selection extensions missing')
    import native_evidence
    native_evidence.configure({k:v['sha256'] for k,v in config['binaries'].items()})


def input_state(config):
    from provenance import sha256, source_tree_digest, verify_build
    import subprocess
    state={}
    for role,source in config['sources'].items():
        path=Path(source['path'])
        head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=path,text=True).strip()
        if head!=source['head']:raise ValueError('source HEAD differs: '+role)
        digest=source_tree_digest(path)
        if digest!=source['source_tree_sha256']:raise ValueError('source bytes differ: '+role)
        state[role]=dict(head=head,source_tree_sha256=digest)
    for role,binary in config['binaries'].items():
        if sha256(Path(binary['path']))!=binary['sha256']:raise ValueError('binary differs: '+role)
        receipt=read(Path(binary['build_receipt']))
        if sha256(Path(binary['build_receipt']))!=binary['build_receipt_sha256'] or receipt['sha256']!=binary['sha256']:
            raise ValueError('binary/build binding differs: '+role)
        if receipt['features']!=['induced-failure'] or receipt['profile']['opt_level']!='3' or receipt['profile']['debug_assertions']:
            raise ValueError('optimized private worker required')
        if receipt['source_tree_sha256']!=state[role]['source_tree_sha256'] or receipt['repository_head']!=state[role]['head']:
            raise ValueError('build/source binding differs: '+role)
        if receipt['cargo_lock_sha256']!=sha256(Path(config['sources'][role]['path'])/'Cargo.lock'):
            raise ValueError('Cargo lock differs: '+role)
        if sha256(Path(receipt['effective_compiler']))!=config['compiler_sha256']:
            raise ValueError('compiler differs: '+role)
        state[role]['binary_sha256']=binary['sha256']
    for name,item in config['tool_files'].items():
        if sha256(Path(name))!=item:raise ValueError('tool bytes differ: '+name)
    import warm_compare
    proof=warm_compare.inspect_fixtures(Path(config['fixtures']),[300,2000,10000])
    if proof!=config['fixture_proof']:raise ValueError('authored fixtures differ')
    return dict(subjects=state,fixtures=proof,tool_files=config['tool_files'])


def measurement_adapter(arm,manifest):
    from transport import Current, resources
    from baseline import append, write_json
    from revalidation import (canonical_prefix,live_answer,validate_ready,validate_history,account,logical_available)
    live=arm==MODE
    class MeasuredCurrent(Current):
        def __init__(self,binary,root,**options):
            self.root_for_ack=root
            self.rows=[None,None]
            self.physical=[]
            self.history=(root/'request-history.jsonl').open('x')
            self.physical_stream=(root/'physical-history.jsonl').open('x')
            self.suffix_count=0
            self.prefix_began=time.perf_counter_ns()
            if live:
                if options.get('loading')!='operator-prefix-live':raise ValueError('wrong live selection')
                (root/'operator-prefix-acks').mkdir()
                options['stage_handler']=self.prefix_stage
            try:
                super().__init__(binary,root,**options)
                parent_ready=time.perf_counter_ns()-self.prefix_began
                if live:
                    coverage=validate_ready(self.stages,self.ready,manifest)
                    validate_history(self.stages,self.ready,parent_ready)
                    for row,event in zip(self.rows,self.stages):
                        if row is not None:row['answer']=copy.deepcopy(event)
                else:
                    from full_intake_control import validate_mode
                    from reference import validate_reference_work, ARMS
                    coverage=validate_mode('main-ordinary',[],self.ready,manifest)
                    validate_reference_work(ARMS[0],self.ready,manifest)
                canonical=canonical_prefix(self,root,manifest,parent_ready)
                if not live:
                    # Both ordinary P2 calls are the logical answers in main.
                    for row in canonical['rows']:row['dispatch']='Host after Ready ordinary prefix'
                prefix=dict(mode=arm,logical_requests=list(COMMANDS),rows=self.rows,
                    attempts=self.stages,physical_history=self.physical,canonical_revalidation=canonical,
                    physical_account=account(self.stages,self.ready) if live else dict(
                        logical_prefix_requests=2,canonical_suffix_requests=481,canonical_host_calls=483,
                        private_request_attempt_events=0,physical_request_attempt_events=483,total_sql_dispatches=483,
                        non_sql_declines=0,fallback_count=0,construction_eligibility_gets=0),
                    first_get_validated_ns=self.rows[0]['parent_validated_ns'],first_find_validated_ns=self.rows[1]['parent_validated_ns'],
                    parent_ready_received_ns=parent_ready,parent_origin_monotonic_ns=self.prefix_began,
                    first_early_response_received_ns=self.stages[0]['parent_received_ns'] if live else None,
                    availability_scope=SCOPE if live else 'ordinary Host after Ready',
                    warm_clocks_include_prefix=False,external_dispatch_identical=False)
                write_json(root/'prefix.json',prefix)
                write_json(root/'ready-validation.json',coverage)
                if live:write_json(root/'loading-checkpoints.json',self.stages)
                self.parent_clock=dict(origin_monotonic_ns=self.prefix_began,
                    first_get_validated_ns=prefix['first_get_validated_ns'],first_find_validated_ns=prefix['first_find_validated_ns'],
                    parent_ready_received_ns=parent_ready,first_early_response_received_ns=prefix['first_early_response_received_ns'],
                    canonical_revalidation_start_ns=canonical['parent_start_ns'],canonical_revalidation_end_ns=canonical['parent_end_ns'],
                    common_search=None,warm_start_parent_ns=None,worker_clocks_separate=True,
                    resource_scope='worker process; excludes controller and fixture generation',controller_cpu_seconds=None)
                write_json(root/'parent-clock.json',self.parent_clock)
            except BaseException:
                if hasattr(self,'process'):self.close()
                else:
                    self.history.close();self.physical_stream.close()
                raise
        def prefix_stage(self,event):
            received=time.perf_counter_ns()-self.prefix_began
            index=event.get('operator_prefix_sequence',0)-1
            if index!=len(self.physical):raise ValueError('missing, duplicate, or reordered phase')
            append(self.physical_stream,dict(kind='private-stage',command=event.get('command'),native_stdout_index=index,
                builder_invoked=event.get('builder_invoked'),disposition=event.get('disposition')))
            normalized=live_answer(index,event,manifest)
            validated=time.perf_counter_ns()-self.prefix_began
            self.physical.append(dict(operation=('get_attempt','find_attempt')[index],dispatch='private',command=COMMANDS[index],
                builder_invoked=event['builder_invoked'],disposition=event['disposition']))
            event.update(parent_received_ns=received,parent_validated_ns=validated,verified=True,
                resources_after_validation=resources(self.process.pid),
                private_storage_at_validation={str(p.relative_to(self.root_for_ack)):p.stat().st_size for p in (self.root_for_ack/'data').rglob('*') if p.is_file()},
                construction_storage_high_water=None,construction_storage_high_water_unavailable='endpoint observations only; no continuous sampler')
            if logical_available(event):
                self.rows[index]=dict(command=COMMANDS[index],answer=None,normalized_answer=normalized,
                    parent_received_ns=received,parent_validated_ns=validated,dispatch='private observer on attachment thread',verified=True)
            pending=self.root_for_ack/'operator-prefix-acks'/f'{index+1}.pending'
            write_json(pending,dict(sequence=index+1,probe=event['probe']))
            pending.replace(pending.with_suffix('.json'))
            event['parent_ack_published_ns']=time.perf_counter_ns()-self.prefix_began
        def call(self,command):
            append(self.history,command)
            requested=time.perf_counter_ns()-self.prefix_began
            if self.suffix_count==1:
                self.parent_clock['warm_start_parent_ns']=requested
                write_json(self.root_for_ack/'parent-clock.json',self.parent_clock)
            append(self.physical_stream,dict(kind='canonical-suffix',command=command,native_stdout_index=len(self.stages)+3+self.suffix_count))
            response,elapsed=super().call(command)
            if self.suffix_count==0:
                from fixture import COMMON
                from results import validate_read
                received=time.perf_counter_ns()-self.prefix_began
                if command!=dict(op='search',query=COMMON):raise ValueError('common search must precede warm operations')
                validate_read('current',command,response,manifest)
                validated=time.perf_counter_ns()-self.prefix_began
                common=dict(parent_origin_monotonic_ns=self.prefix_began,command=command,
                    parent_requested_ns=requested,parent_received_ns=received,parent_validated_ns=validated,
                    round_trip_ns=elapsed,validation_ns=validated-received,native_stdout_index=len(self.stages)+3,
                    resources_after_validation=resources(self.process.pid))
                write_json(self.root_for_ack/'common-search-endpoint.json',common)
                self.parent_clock['common_search']=common
                write_json(self.root_for_ack/'parent-clock.json',self.parent_clock)
            self.suffix_count+=1
            return response,elapsed
        def close(self):
            try:super().close()
            finally:
                self.history.close();self.physical_stream.close()
    return MeasuredCurrent


def validate_parent_clock(root,prefix):
    from operator_prefix import integer
    clock=json.loads((root/'parent-clock.json').read_text())
    if (integer(clock.get('origin_monotonic_ns')) == 0 or clock.get('worker_clocks_separate') is not True
            or clock.get('resource_scope') != 'worker process; excludes controller and fixture generation'
            or clock.get('controller_cpu_seconds','missing') is not None):
        raise ValueError('parent origin or worker/controller resource scope differs')
    expected=dict(first_get_validated_ns=prefix['rows'][0]['parent_validated_ns'],
        first_find_validated_ns=prefix['rows'][1]['parent_validated_ns'],
        parent_ready_received_ns=prefix['parent_ready_received_ns'])
    attempts=prefix.get('attempts',[])
    expected['first_early_response_received_ns']=attempts[0]['parent_received_ns'] if attempts else None
    canonical=prefix.get('canonical_revalidation',{})
    expected.update(canonical_revalidation_start_ns=canonical.get('parent_start_ns'),
                    canonical_revalidation_end_ns=canonical.get('parent_end_ns'))
    if any(clock.get(k) != v for k,v in expected.items()):
        raise ValueError('parent endpoints differ from actual logical prefix/Ready/revalidation receipts')
    if prefix.get('parent_origin_monotonic_ns',clock['origin_monotonic_ns']) != clock['origin_monotonic_ns']:
        raise ValueError('canonical prefix uses a different parent origin')
    common=clock.get('common_search')
    if (root/'common-search-endpoint.json').is_file():
        if common != json.loads((root/'common-search-endpoint.json').read_text()):
            raise ValueError('common-search parent clock differs from retained endpoint')
    else:
        raise ValueError('missing common-search producer endpoint')
    if not isinstance(common,dict):
        raise ValueError('missing common-search parent clock')
    requested,received,validated=(integer(common.get(k)) for k in ('parent_requested_ns','parent_received_ns','parent_validated_ns'))
    floor=max(expected['parent_ready_received_ns'],expected['first_get_validated_ns'],expected['first_find_validated_ns'],
              canonical.get('parent_end_ns',0))
    if not floor <= requested <= received <= validated <= integer(clock.get('warm_start_parent_ns')):
        raise ValueError('common-search or warm-start parent clock order differs')
    return clock


def compare_cells(cells):
    """Retain E's complete native, source and final-index equality guards."""
    reference = {}
    for cell in cells:
        if cell.get('completed') is not True or cell.get('integrity_verified') is not True:
            raise ValueError('incomplete or unverified cell')
        value = {key:cell[key] for key in ('native_answers','normalized_prefix','source',
                 'semantic_sha256','canonical_identity_sha256','page_map_sha256','document_construction_sha256',
                 'comparable_writer','history','canonical_prefix_answers','canonical_history')}
        size = cell['cell']['background_documents']
        if reference.setdefault(size,value) != value:
            raise ValueError('native answer/counter, source, semantics, writer, history or index identity differs')




def verify_cell(root,arm,fixtures,config,*,size=300):
    from native_evidence import replay
    from native_validation import validate_native_cell
    from provenance import sha256
    import warm_compare as frozen
    from revalidation import live_answer, canonical_answer, validate_ready
    from full_intake_control import validate_mode
    from work_account import WRITER_WORK
    records=[json.loads(line) for line in (root.parents[1]/'samples.jsonl').read_text().splitlines()]
    manifest=read(fixtures/str(size)/'manifest.json')
    selected=dict(arm=arm,subject='candidate' if arm==MODE else 'baseline',loading='operator-prefix-live' if arm==MODE else None)
    audit=replay(root,selected,dict(background_documents=size),fixtures,manifest,read(root/'final-source.json'))
    native=validate_native_cell(root,records,manifest,audit,mode=arm)
    history=[json.loads(line) for line in (root/'request-history.jsonl').read_text().splitlines()]
    prefix=read(root/'prefix.json')
    stages=read(root/'loading-checkpoints.json') if arm==MODE else []
    coverage=validate_ready(stages,records[0]['ready'],manifest) if arm==MODE else validate_mode(arm,[],records[0]['ready'],manifest)
    if read(root/'ready-validation.json')!=coverage:raise ValueError('Ready receipt differs')
    ready=records[0]['ready']
    work=ready['operator_prefix_receipt']['canonical_work'] if arm==MODE else ready['job_evidence']
    writer={k:work.get(k) for k in WRITER_WORK}
    unreported=[k for k,v in writer.items() if v is None]
    if unreported!=(['tombstones_recorded'] if arm=='main-ordinary' and 'tombstones_recorded' not in work else []):
        raise ValueError('canonical writer work vocabulary differs')
    frozen.validate_projection_residue(root)
    normalized=[]
    for i,row in enumerate(prefix['rows']):
        response=row['answer']
        if 'startup_stage' in response:
            value=live_answer(i,response,manifest)
            if response['disposition']!='served':raise ValueError('withdrawn logical answer cannot be available')
        else:
            value,_=canonical_answer(i,response,manifest,expected=(audit['store_epoch'],audit['write_generation']-120))
        if value!=row['normalized_answer']:raise ValueError('saved logical answer normalization differs')
        normalized.append(value)
    clock=validate_parent_clock(root,prefix)
    for role in ('worker','integrity','audit'):
        receipt=read(root/f'{role}-process.json')
        if not (receipt['returncode']==0 and receipt['process_closed'] and receipt['group_closed']):
            raise ValueError('native producer did not exit/reap')
    if read(root/'worker-process.json')['started_monotonic_ns']<clock['origin_monotonic_ns']:
        raise ValueError('parent origin must precede worker transport')
    if records[-1]['final_tree_sha256']!=read(root/'final-source.json')['tree_sha256']:
        raise ValueError('source changed after worker close')
    quality={k:audit[k] for k in ('semantic_sha256','canonical_identity_sha256','page_map_sha256','document_construction_sha256')}
    if any(not isinstance(v,str) or len(v)!=64 for v in quality.values()):raise ValueError('missing final quality identity')
    return dict(completed=True,integrity_verified=True,native_answers=native,normalized_prefix=normalized,
        source=read(root/'final-source.json'),comparable_writer={k:v for k,v in writer.items() if k!='tombstones_recorded'},
        canonical_writer_work=writer,unreported_counters=unreported,history=history,**quality,
        operations=frozen.summarize(records),measurement_clock=clock,physical_account=prefix['physical_account'],
        canonical_prefix_answers=[dict(answer=canonical_answer(i,row['answer'],manifest,
            expected=(audit['store_epoch'],audit['write_generation']-120))[0],counters=row['answer']['counters'])
            for i,row in enumerate(prefix['canonical_revalidation']['rows'])],
        canonical_history=list(COMMANDS)+history,
        native_stdout_sha256=sha256(root/'worker.stdout.log'),clocks_qualifying=False,performance_claim=False)


def plan(config,output,config_path=None):
    cells=[]
    for pair,size,arms in ((0,2000,('main-ordinary',MODE)),(0,10000,('main-ordinary',MODE)),
                           (1,10000,(MODE,'main-ordinary')),(1,2000,(MODE,'main-ordinary'))):
        for arm in arms:
            index=len(cells)
            cells.append(dict(index=index,pair=pair,background_documents=size,documents=size+14,arm=arm,
                output=str(output/f'cell-{index:02d}-{size+14}-{arm}'),
                fixture=config['fixtures']+'/'+str(size),status='planned-not-executed',
                argv=[sys.executable,str(Path(__file__).resolve()),'--config',str(config_path),
                      '--output',str(output/f'cell-{index:02d}-{size+14}-{arm}'),'--arm',arm,
                      '--size',str(size),'--execute-cell']))
    return dict(cells=cells,n_pairs_per_size=2,workers_executed=False,
        outcome_policy='retain every outcome; stop at first failed/interrupted cell pending parent disposition; never repeat a completed cell',
        execution_policy='serial processes; close all owned heavy jobs before sampling; ambient T3 Git polling remains',
        interpretation='descriptive only; cold gains and exact results mandatory; warm differences for judgment; no 1% gate, independent-call assumption, confidence intervals, A/A subtraction, or statistical pass',
        cache_policy='OS caches uncontrolled; no selective reruns',baseline_calls=483,successful_candidate_calls=485,
        canonical_calls_per_arm=483,unchanged_suffix_calls=481)


def run_cell(config,arm,output,*,size=300):
    import baseline
    import warm_compare as frozen
    from baseline import write_json
    from native_evidence import Capture
    from startup import storage_bytes
    if (platform.system()!='Linux' or any(k in os.environ for k in ('TMPDIR','NORN_TEST_ISOLATION_DIR'))
            or Path.home()!=Path(pwd.getpwuid(os.getuid()).pw_dir)):
        raise ValueError('normal HOME, Linux and existing watcher lease required')
    if output.exists() or not output.is_relative_to(Path.home()):raise ValueError('fresh output below home required')
    protected=[Path(config[k]) for k in ('fixtures','existing_bundle','performance_tools','prototype_tools')]
    protected += [Path(v['path']) for v in config['sources'].values()]
    if any(output.is_relative_to(p) or p.is_relative_to(output) for p in protected):raise ValueError('output overlaps input')
    ancestor=output.parent
    while not ancestor.exists():ancestor=ancestor.parent
    if shutil.disk_usage(ancestor).free<50*1024**3:raise ValueError('50 GiB reserve required')
    before=input_state(config)
    output.mkdir(parents=True)
    summary=dict(completed=False,arm=arm,documents=size+14,clocks_qualifying=False,performance_claim=False,
        inputs=before,argv=sys.argv,started_monotonic_ns=time.perf_counter_ns())
    original=baseline.Current
    try:
        fixture,manifest=baseline.prepare(SimpleNamespace(output=output,fixtures=Path(config['fixtures'])),size)
        baseline.Current=measurement_adapter(arm,manifest)
        root=output/'work'/f'{size}-0-c'
        candidate=Path(config['binaries']['candidate']['path']);main=Path(config['binaries']['baseline']['path'])
        with Capture(root,manifest['tree_sha256'],(candidate,main)) as capture:
            with (output/'samples.jsonl').open('x') as samples:
                baseline.run_worker(SimpleNamespace(output=output,current=candidate if arm==MODE else main,
                    loading='operator-prefix-live' if arm==MODE else None,warmups=5,samples=25),
                    'current',size,0,fixture,manifest,samples)
            capture.flush()
            summary['post_close_storage_bytes']=storage_bytes(root)
            frozen.final_audit(candidate,root)
            capture.flush()
        summary.update(verify_cell(root,arm,Path(config['fixtures']),config,size=size))
        if input_state(config)!=before:raise ValueError('inputs changed around functional cell')
    except BaseException as problem:
        summary.update(completed=False,failure=repr(problem))
        raise
    finally:
        baseline.Current=original
        summary['finished_monotonic_ns']=time.perf_counter_ns()
        write_json(output/'run.json',summary)
    return summary


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--arm',choices=('main-ordinary',MODE),default=MODE)
    parser.add_argument('--prepare-plan',action='store_true')
    parser.add_argument('--size',type=int,choices=(300,2000,10000),default=300)
    parser.add_argument('--execute-functional',action='store_true')
    parser.add_argument('--execute-cell',action='store_true')
    parser.add_argument('--compare',nargs=2,type=Path)
    args=parser.parse_args()
    config=read(args.config.resolve());configure(config)
    if args.prepare_plan:
        if args.execute_functional or args.execute_cell:raise ValueError('plan preparation launches no workers')
        from baseline import write_json
        if args.output.exists():raise ValueError('fresh plan file required')
        write_json(args.output,plan(config,args.output.parent/'planned-cells',args.config.resolve()))
        return
    if args.compare:
        summaries=[read(p/'run.json') for p in args.compare]
        compare_cells([s|{'cell':dict(background_documents=300)} for s in summaries])
        from baseline import write_json
        write_json(args.output,dict(result='PASS',documents=314,full_canonical_history_answers_counters_source_writer_and_four_identities_equal=True,
            cells=[str(p) for p in args.compare],performance_claim=False,clocks_qualifying=False))
        return
    state=input_state(config)
    if args.execute_functional and args.size!=300:raise ValueError('functional proof uses only 314 documents')
    if not (args.execute_functional or args.execute_cell):
        print(json.dumps(dict(preflight='PASS',workers_executed=False,inputs=state)))
        return
    run_cell(config,args.arm,args.output.resolve(),size=args.size)
    print(f'{args.size+14}-document cell PASS; descriptive clocks only')


if __name__=='__main__':main()
