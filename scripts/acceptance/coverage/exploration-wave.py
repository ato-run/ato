#!/usr/bin/env python3
"""Actual product CLI/Coordinator/Runtime measurement. No provider transport.

All model sends and accounting belong to the product requester. This harness
only verifies frozen evidence, starts isolated services and collects raw output.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import time
from urllib.parse import urlparse
from urllib.request import Request, urlopen


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def read(p):
    return json.loads(Path(p).read_text())


def write(p, v):
    with Path(p).open('x') as f:
        json.dump(v, f, indent=2)
        f.write('\n')
        f.flush()
        os.fsync(f.fileno())


def require(ok, reason):
    if not ok:
        raise RuntimeError(reason)


class Wave:
    def __init__(self, a):
        self.a = a
        self.plan = read(a.plan)
        require(sha(a.plan) == a.plan_sha256, 'plan changed')
        require(self.plan['maximum_reservation_usd_micros'] <= 3507128, 'reservation ceiling')
        for value in self.plan['binaries'].values():
            require(sha(value['path']) == value['sha256'], 'binary changed')
        for name, digest in self.plan['receiver_hashes'].items():
            require(sha(Path(self.plan['receiver_bundle']) / name) == digest, 'receiver changed')
        for app in self.plan['applications']:
            require(sha(app['archive_path']) == app['archive_sha256'], 'source changed')
        require(shutil.disk_usage('/home/ubuntu').free >= 20 * 1024**3, 'insufficient disk')
        self.root = Path(a.run)
        self.suffix = 'resume'+str(1+len(list(self.root.glob('coordinator.resume*.log')))) if a.resume else ''
        self.processes = []
        self.env = {'PATH':'/opt/ato/toolchains/node/22.14.0/bin:/usr/bin:/bin',
                    'HOME':'/home/ubuntu', 'TMPDIR':str(self.root / '.tmp')}
        api = urlparse(self.plan['api'])
        require(api.hostname == '127.0.0.1' and api.port, 'isolated local Coordinator required')
        self.env['C2_PORT'] = str(api.port)

    def start(self, cmd, cwd, stdout, stderr=None):
        out = Path(stdout).open('xb')
        err = Path(stderr).open('xb') if stderr else out
        try:
            p = subprocess.Popen(list(map(str,cmd)), cwd=cwd, env=self.env,
                                 stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                 close_fds=True, start_new_session=True)
        finally:
            out.close()
            if err is not out:
                err.close()
        self.processes.append(p)
        return p

    def stop(self, p):
        if p.poll() is None:
            os.killpg(p.pid, signal.SIGTERM)
            try:
                p.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()

    def wait(self, predicate, reason):
        end = time.monotonic() + 60
        while time.monotonic() < end:
            if predicate():
                return
            time.sleep(.1)
        raise RuntimeError(reason)

    def sql(self, sql, params=None):
        name = secrets.token_hex(8)
        command = self.root/'receiver'/'commands'/name
        write(command.with_suffix('.in.json'), {'sql':sql,'params':params or []})
        out = command.with_suffix('.out.json')
        self.wait(out.exists, 'local SQL channel')
        result = read(out)
        require('error' not in result, 'local SQL failed')
        return result.get('results', [])

    def setup(self):
        self.root.mkdir(exist_ok=self.a.resume)
        (self.root/'.tmp').mkdir(exist_ok=self.a.resume)
        (self.root/'cells').mkdir(exist_ok=self.a.resume)
        receiver = self.root/'receiver'
        receiver.mkdir(exist_ok=self.a.resume)
        shutil.copytree(self.plan['receiver_bundle'], receiver/'bundle',dirs_exist_ok=self.a.resume)
        shutil.copy2(self.plan['coordinator_script'], receiver/'coordinator.mjs')
        if not (receiver/'node_modules').exists():
            (receiver/'node_modules').symlink_to(self.plan['api_node_modules'])
        coordinator = self.start(['node','coordinator.mjs'], receiver, self.root/('coordinator.'+self.suffix+'.log' if self.a.resume else 'coordinator.log'))
        self.wait(lambda: (receiver/'commands').exists() and coordinator.poll() is None,
                  'Coordinator initialization')
        self.token = self.root/'coordinator-token'
        if not self.a.resume:
            self.token.write_text('ato_rnr_'+secrets.token_hex(32))
            self.token.chmod(0o600)
            self.sql('INSERT INTO "user"(id,name,email) VALUES(?,?,?)',
                     ['exploration','exploration','exploration@acceptance.invalid'])
            self.sql('INSERT INTO runner_devices(id,user_id,display_name,token_hash) VALUES(?,?,?,?)',
                     ['local','exploration','isolated exploration',sha(self.token)])
            write(self.root/'sandbox.json', self.plan['sandbox'])
        else:
            require(read(self.root/'sandbox.json') == self.plan['sandbox'], 'sandbox changed on restart')
        def http_ready():
            try:
                req = Request(self.plan['api']+'/v1/runtime-network/runtimes', headers={
                    'Authorization':'Bearer '+self.token.read_text().strip()})
                with urlopen(req,timeout=1) as response:
                    return response.status == 200
            except Exception:
                return False
        self.wait(http_ready,'Coordinator authenticated HTTP readiness')
        runtime_root = Path(self.a.runtime_root)
        runtime_root.mkdir(exist_ok=self.a.resume, parents=True)
        self.runtime = self.start([self.plan['binaries']['ato']['path'],'runtime-network','serve',
            '--api',self.plan['api'],'--token-file',self.token,'--work-root',runtime_root/'w',
            '--out',runtime_root/'out','--exploration-sandbox',self.root/'sandbox.json'],
            self.root,self.root/('runtime.'+self.suffix+'.log' if self.a.resume else 'runtime.log'))
        self.wait(lambda: self.sql('SELECT environment_id FROM runtime_environments WHERE runtime_id=?', ['local']),
                  'Runtime advertisement')

    def app(self, app):
        n = f"{app['index']:03}"
        cell = self.root/'cells'/n
        continuation = self.a.resume and cell.exists()
        cell.mkdir(exist_ok=continuation)
        config = self.plan['preflight_config']
        if not continuation:
            write(cell/'preflight-config.json',config)
            preflight = self.start([self.plan['binaries']['preflight']['path'],app['archive_path'],
                'sha256:'+app['archive_sha256'],cell/'scratch','search_preregister_'+n,
                cell/'preflight-config.json',cell/'preflight.json'], self.root,
                cell/'preflight.stdout.log',cell/'preflight.stderr.log')
            require(preflight.wait(timeout=120) == 0, 'verified-source preflight failure')
        projected = read(cell/'preflight.json')
        require(projected['contract_ref'] == app['contract_ref'], 'K changed')
        cp = self.plan['producer_config']
        live = {'schema':'ato.formation-exploration-config/1','contract':projected['contract'],
                'exploration':config['exploration'],'toolchains':config['toolchains'],
                'provider':cp,'provider_budget':self.plan['producer_budget'],
                'credential_environment':'DEEPSEEK_API_KEY','decision':self.plan['decision_config'],
                'provider_journal':str(cell/'producer.jsonl')}
        if not continuation:
            write(cell/'config.json',live)
        else:
            require(read(cell/'config.json') == live, 'requester config changed on restart')
        cmd = [self.plan['binaries']['ato']['path'],'form',projected['source'],'--runtime-network',
            '--exploration-config',cell/'config.json','--api',self.plan['api'],
            '--token-file',self.token,'--exact-runtime','local','--network','denied',
            '--work-root',cell/'requester','--max-attempts','4','--deadline-seconds','900',
            '--max-transfer-bytes',str(1024**3),'--max-expanded-bytes',str(2*1024**3),
            '--max-stored-bytes',str(1024**3)]
        if continuation:
            cmd.extend(['--search-id',read(cell/'producer.search.json')['search_id']])
        if self.a.credential_socket:
            cmd = ['python3',self.plan['credential_wrapper'],self.a.credential_socket,*cmd]
        start = time.monotonic()
        output = cell/('requester.'+self.suffix+'.stdout.log' if continuation else 'requester.stdout.log')
        requester = self.start(cmd,self.root,output,cell/('requester.'+self.suffix+'.stderr.log' if continuation else 'requester.stderr.log'))
        rc = requester.wait(timeout=950)
        result = read(output)
        require(result['contract_ref'] == app['contract_ref'], 'result K mismatch')
        require(result['approval'] == 'not_assessed' and result['deployed'] is False, 'submission confused with approval')
        cp_journal = result['candidate_producer_accounting']
        dp_journal = result['decision_provider_accounting'].get('journal',{})
        row = {'index':app['index'],'name':app['name'],'baseline_typed_K_pass':app['baseline_typed_K_pass'],
               'typed_K_pass':result['submission'] is not None,'rounds_consumed':result['rounds_consumed'],
               'producer_calls':len(cp_journal['cells']),'decision_calls':len(dp_journal.get('cells',{})),
               'result':str(output.relative_to(self.root)),'result_sha256':sha(output),
               'exit_code':rc,'elapsed_seconds':round(time.monotonic()-start,3),
               'functional_acceptance':'not_measured'}
        write(cell/('summary.'+self.suffix+'.json' if continuation else 'summary.json'),row)
        print(json.dumps(row),flush=True)
        # Source copies are disposable; preserve raw evidence, journals, source
        # archives, worker state and receipts, including any unresolved UNKNOWN.
        shutil.rmtree(cell/'scratch')
        shutil.rmtree(cell/'requester')

    def run(self):
        if self.a.check_only:
            print('NON_SECRET_GATE_PASS',flush=True)
            return
        try:
            self.setup()
            selected = set(map(int,self.a.indices.split(',')))
            for app in self.plan['applications']:
                if app['index'] in selected:
                    self.app(app)
        finally:
            for p in reversed(self.processes):
                self.stop(p)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--plan',required=True)
    parser.add_argument('--plan-sha256',required=True)
    parser.add_argument('--run',required=True)
    parser.add_argument('--runtime-root',required=True)
    parser.add_argument('--indices',required=True)
    parser.add_argument('--check-only',action='store_true')
    parser.add_argument('--credential-socket')
    parser.add_argument('--resume',action='store_true')
    Wave(parser.parse_args()).run()
