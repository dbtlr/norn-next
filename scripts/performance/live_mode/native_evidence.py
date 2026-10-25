"""Capture closed native producers and replay the existing ordered audit, without SQL."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

BINARY_SHA256 = {}


def configure(binaries):
    """Concrete executable pins come from this adapter's own configuration."""
    global BINARY_SHA256
    BINARY_SHA256 = dict(binaries)


ROLES={'serve':'worker','verify':'integrity','audit':'audit'}

def environment_snapshot(env):
    names={'HOME','PATH','TMPDIR','NORN_TEST_ISOLATION_DIR'}
    return {k:v for k,v in sorted(env.items()) if k in names or k.startswith(('NORN_','XDG_'))}


def identity(root):
    from revalidation import root_identity
    return dict(cell_root=str(root),root_identity=root_identity(root),
                cell_token=(root/'performance-token').read_text())


class Capture:
    """Observe the actual Popen boundary; delegate launch, transport and cleanup unchanged."""
    def __init__(self,root,initial_source,binaries):
        self.root=root
        from provenance import sha256 as sha
        self.initial_source=initial_source
        # Hash both fixed binaries before the measured constructor, equally for
        # every arm. The existing pre/post guard remains authoritative.
        self.binary_hashes={str(path):sha(path) for path in binaries}
        self.observed=[]

    def __enter__(self):
        self.original=subprocess.Popen
        def launch(command,*pos,**options):
            from provenance import sha256 as sha
            if Path(options.get('cwd','')).resolve() != self.root.resolve():
                return self.original(command,*pos,**options)
            if not isinstance(command,list) or len(command)!=2 or command[1] not in ROLES:
                raise ValueError('unexpected native producer command')
            role=ROLES[command[1]]
            if any(r['role']==role for _,r in self.observed):
                raise ValueError('duplicate native producer')
            # Observe each prior process after its unchanged synchronous close,
            # before the next native launch. End receipts are actual closed
            # observations, not a timestamp synthesized after a later audit.
            self.flush()
            began=time.perf_counter_ns()
            # baseline.run_worker has just verified the actual copied initial
            # source. E writes final-source before its verify/audit commands.
            # Reuse those receipts here; do not scan/warm the corpus at spawn.
            source=self.initial_source if role=='worker' else json.loads((self.root/'final-source.json').read_text())['tree_sha256']
            environment_sha256=hashlib.sha256(json.dumps(options['env'],sort_keys=True).encode()).hexdigest()
            launch_cwd=str(Path(options['cwd']).resolve())
            process=self.original(command,*pos,**options)
            receipt=dict(schema='native-closed-producer-r2-v1',role=role,command=list(command),
                binary_sha256=self.binary_hashes[command[0]],parent_pid=os.getpid(),pid=process.pid,
                started_monotonic_ns=began,source_sha256=source,
                environment=environment_snapshot(options['env']),environment_sha256=environment_sha256,cwd=launch_cwd,**identity(self.root))
            self.observed.append((process,receipt))
            return process
        subprocess.Popen=launch
        return self

    def flush(self):
        from baseline import write_json
        from provenance import sha256 as sha
        from transport import group_running
        for process,receipt in self.observed:
            code=process.poll()
            if code is None or group_running(process.pid):
                raise ValueError('native producer/group has not closed')
            if 'finished_monotonic_ns' not in receipt:
                receipt.update(finished_monotonic_ns=time.perf_counter_ns(),returncode=code,
                               process_closed=True,group_closed=True,
                               streams={name:dict(sha256=sha(self.root/f"{receipt['role']}.{name}.log"),
                                                 bytes=(self.root/f"{receipt['role']}.{name}.log").stat().st_size)
                                        for name in ('stdout','stderr')})
            write_json(self.root/f"{receipt['role']}-process.json",receipt)

    def __exit__(self,kind,value,trace):
        subprocess.Popen=self.original
        if kind is None:
            self.flush()


def expected_environment(root,role,loading):
    from transport import environment
    env=environment(root,(root/'performance-token').read_text())
    if role=='worker':
        env.update(NORN_PERFORMANCE_CHANGESET_SIZE='128',NORN_PERFORMANCE_PROFILE='0')
        if loading:
            env['NORN_PERFORMANCE_LOADING']=loading
            if loading=='operator-prefix-capped':env['NORN_PERFORMANCE_FROZEN_INPUT']='frozen-fixture-v1'
    return environment_snapshot(env)


def validate_process(root,role,subject,source,loading=None,*,producer=None):
    from controller import read
    from provenance import sha256 as sha
    from operator_prefix import integer
    receipt=read(root/f'{role}-process.json')
    command=receipt.get('command')
    verb=next(k for k,v in ROLES.items() if v==role)
    if (not isinstance(command,list) or len(command)!=2 or command[1]!=verb
            or receipt.get('schema')!='native-closed-producer-r2-v1' or receipt.get('role')!=role
            or any(receipt.get(k)!=v for k,v in identity(root).items())
            or receipt.get('source_sha256')!=source
            or receipt.get('binary_sha256')!=BINARY_SHA256[subject]
            or not Path(command[0]).is_file() or sha(Path(command[0]))!=BINARY_SHA256[subject]
            or receipt.get('environment')!=expected_environment(root,role,loading)
            or type(receipt.get('returncode')) is not int or receipt['returncode']!=0
            or receipt.get('process_closed') is not True or receipt.get('group_closed') is not True):
        raise ValueError(f'{role} actual native producer command/source/environment/status differs')
    if 'cwd' in receipt and receipt['cwd']!=str(root.resolve()):raise ValueError('native producer cwd differs')
    if 'environment_sha256' in receipt:
        from transport import environment
        env=environment(root,(root/'performance-token').read_text())
        if role=='worker':
            env.update(NORN_PERFORMANCE_CHANGESET_SIZE='128',NORN_PERFORMANCE_PROFILE='0')
            if loading:env['NORN_PERFORMANCE_LOADING']=loading
            if loading=='operator-prefix-capped':env['NORN_PERFORMANCE_FROZEN_INPUT']='frozen-fixture-v1'
        if receipt['environment_sha256']!=hashlib.sha256(json.dumps(env,sort_keys=True).encode()).hexdigest():
            raise ValueError('exact native producer environment fingerprint differs')
    began=integer(receipt.get('started_monotonic_ns'));ended=integer(receipt.get('finished_monotonic_ns'))
    if not 0 < began <= ended or integer(receipt.get('pid'))==0 or integer(receipt.get('parent_pid'))==0:
        raise ValueError(f'{role} producer origin/completion differs')
    if producer is not None:
        native=producer['configuration']['native'][subject]
        if (command[0]!=native['path'] or receipt['parent_pid']!=producer['controller_pid']
                or began<producer['started_monotonic_ns']
                or ended>producer.get('finished_monotonic_ns',ended)):
            raise ValueError(f'{role} producer is not owned by the bound controller execution')
    actual={name:dict(sha256=sha(root/f'{role}.{name}.log'),bytes=(root/f'{role}.{name}.log').stat().st_size) for name in ('stdout','stderr')}
    if receipt.get('streams')!=actual or (root/f'{role}.stderr.log').read_bytes():
        raise ValueError(f'{role} retained stdout/stderr differs from closed producer')
    return receipt


def validate_authored_source(root,fixtures,size,manifest):
    """Replay authored bytes on only touched files; unchanged corpus bytes stay independent."""
    from warm_compare import expected_warm_history,validate_warm_history
    from controller import read
    history=[json.loads(s) for s in (root/'request-history.jsonl').read_text().splitlines()]
    validate_warm_history(history)
    original=fixtures/str(size)/'vault';vault=root/'vault'
    state={}
    def content(path):
        if path not in state:state[path]=(original/path).read_bytes()
        if state[path] is None:raise ValueError('authored mutation reads removed source')
        return state[path]
    for command in history:
        op=command['op']
        if op=='set':
            path='bench/subject.md';before=content(path);lines=before.splitlines(keepends=True)
            at=next(i for i,line in enumerate(lines) if line.startswith(b'status:'))
            lines[at]=f"status: {command['value']}\n".encode();state[path]=b''.join(lines)
        elif op=='new':state[command['path']]=command['content'].encode()
        elif op=='move':
            src,dst=command['from'],command['to'];state[dst]=content(src);state[src]=None
            if 'hub' in dst:
                for n in range(10):
                    path=f'bench/referrer-{n:02}.md'
                    state[path]=content(path).replace(
                        ('[['+src.removesuffix('.md')+']]').encode(),
                        ('[['+dst.removesuffix('.md')+']]').encode())
    expected=(set(manifest['paths'])|{k for k,v in state.items() if v is not None})-{k for k,v in state.items() if v is None}
    actual=sorted(p.relative_to(vault).as_posix() for p in vault.rglob('*.md'))
    if actual!=sorted(expected):raise ValueError('final source paths differ from authored mutations')
    for path in actual:
        want=state[path] if path in state else (original/path).read_bytes()
        data=(vault/path).read_bytes()
        if path=='bench/subject.md':
            # The native YAML scalar writer may preserve or quote the scalar.
            options=[want,want.replace(b'status: value29\n',b'status: "value29"\n'),want.replace(b'status: value29\n',b"status: 'value29'\n")]
            if data not in options:raise ValueError('final source field bytes differ from authored mutation')
        elif data!=want:raise ValueError(f'final source bytes differ from original/authored mutation: {path}')
    return actual


def replay(root,selected,cell,fixtures,manifest,final,*,producer=None):
    from controller import read
    from transport import PREFIX
    from warm_compare import validate_integrity
    from fixture import tree_digest
    paths=validate_authored_source(root,fixtures,cell['background_documents'],manifest)
    source=tree_digest(root/'vault')
    audit_receipt=validate_process(root,'audit','candidate',source,producer=producer)
    integrity_receipt=validate_process(root,'integrity','candidate',source,producer=producer)
    worker=validate_process(root,'worker',selected['subject'],manifest['tree_sha256'],selected['loading'],producer=producer)
    if not worker['finished_monotonic_ns']<=integrity_receipt['started_monotonic_ns']<=integrity_receipt['finished_monotonic_ns']<=audit_receipt['started_monotonic_ns']:
        raise ValueError('native worker/integrity/audit producer order differs')
    validate_integrity((root/'integrity.stdout.log').read_text(),integrity_receipt['returncode'])
    documents=[];summary=None
    raw=(root/'audit.stdout.log').read_bytes()
    if not raw or not raw.endswith(b'\n'):raise ValueError('partial or missing raw native audit stream')
    for line in raw.decode().splitlines():
        if not line.startswith(PREFIX):raise ValueError('unexpected raw native audit record')
        row=json.loads(line[len(PREFIX):])
        if summary is not None or not isinstance(row,dict):raise ValueError('native audit record after summary')
        if set(row)=={'document'}:
            d=row['document'];index=len(documents)
            if (not isinstance(d,dict) or set(d)!={'path','content_hash','byte_length'}
                    or index>=len(paths) or d['path']!=paths[index]):raise ValueError('native audit document order/shape differs')
            content=(root/'vault'/paths[index]).read_bytes()
            if (d['content_hash']!=hashlib.sha256(content).hexdigest() or type(d['byte_length']) is not int
                    or d['byte_length']!=len(content)):raise ValueError('native audit document hash/length differs from authored final source')
            documents.append(d)
        elif set(row)=={'summary'}:summary=row['summary']
        else:raise ValueError('unexpected native audit record shape')
    if (len(documents)!=len(paths) or not isinstance(summary,dict) or summary.get('schema_version')!=3
            or 'source_sha256' in summary):
        raise ValueError('incomplete ordered native audit stream')
    if (summary.get('counts',{}).get('documents')!=len(paths) or summary.get('tables',{}).get('documents',{}).get('rows')!=len(paths)
            or final['paths']!=paths or final['markdown_bytes']!=sum(d['byte_length'] for d in documents)
            or final['tree_sha256']!=source):raise ValueError('native audit counts/source manifest differs')
    audit=summary|{'source_sha256':source}
    # Exact derived JSON equality includes all tables, work, page map and quality
    # fields returned by the native auditor, not just the four comparison keys.
    if read(root/'final-audit.json')!=audit:raise ValueError('derived final audit differs from actual ordered native producer stream')
    return audit
