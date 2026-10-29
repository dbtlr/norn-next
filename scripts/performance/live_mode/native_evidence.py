"""Capture closed native producers and replay the existing ordered audit, without SQL."""
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import time

BINARY_SHA256 = {}


def read(path):return json.loads(path.read_text())


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
    return environment_snapshot(env)


def validate_process(root,role,subject,source,loading=None,*,producer=None):
    from provenance import sha256 as sha
    from revalidation import integer
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
    # The complete digest identifies the launch environment, including opaque
    # harness call IDs. A later replay's ambient environment is a different
    # observation. Operational values remain checked above against their pins.
    if 'environment_sha256' in receipt and (
            not isinstance(receipt['environment_sha256'],str)
            or re.fullmatch('[0-9a-f]{64}',receipt['environment_sha256']) is None):
        raise ValueError('invalid original producer environment fingerprint')
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
