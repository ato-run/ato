"""Bounded local Coordinator/Runtime control. No model client or key reads.

Not reachable through the CLI while the immutable request-capture blocker is
open. Evidence is operational; the pinned Rust requester alone accepts routes.
"""
import hashlib
import json
import os
import pathlib
import secrets
import shutil
import signal
import subprocess
import time
from .preflight import Stop, require, write_new, verify_binaries, approved_plan, helper, load, digest
from .gates import journal_state, cell_result
from .proxy import create_proxy


class LocalRun:
    def __init__(self,args,manifest):
        self.args=args
        self.manifest=manifest
        self.processes=[]
        self.receiver=args.run/'receiver'
        self.receiver.mkdir()
        self.scratch=args.run/'scratch';self.scratch.mkdir()
        self.home=args.run/'home';self.home.mkdir()
        self.port=19544
        # Do not copy/enumerate the controller environment. Only requester gets
        # ordinary environment inheritance; Runtime/Coordinator get this map.
        self.environment={'PATH':'/opt/ato/toolchains/node/22.14.0/bin:/opt/ato/toolchains/python/3.12.7/bin:/usr/local/bin:/usr/bin:/bin',
                          'HOME':str(self.home),'TMPDIR':str(self.scratch)}

    def start(self,argv,log,cwd=None,requester=False):
        verify_binaries(self.manifest)
        with log.open('xb') as stream:
            proc=subprocess.Popen([str(x) for x in argv],cwd=cwd,env=None if requester else self.environment,
                                  stdout=stream,stderr=subprocess.STDOUT,start_new_session=True)
        self.processes.append(proc)
        return proc

    def stop(self,proc):
        if proc.poll() is None:
            os.killpg(proc.pid,signal.SIGTERM)
            try:proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid,signal.SIGKILL);proc.wait(timeout=10)

    def close(self):
        for proc in reversed(self.processes):self.stop(proc)

    def wait(self,fn,message,seconds=60):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            value=fn()
            if value:return value
            time.sleep(.05)
        raise Stop(message)

    def sql(self,sql,params):
        folder=self.receiver/'commands'
        name=secrets.token_hex(12)
        tmp=folder/(name+'.tmp');write_new(tmp,dict(sql=sql,params=params));tmp.rename(folder/(name+'.in.json'))
        out=folder/(name+'.out.json')
        self.wait(out.exists,'local D1 timeout')
        result=load(out)
        require('error' not in result,'local D1 error')
        # SQL parameters include only the local Coordinator credential hash,
        # never any external provider credential.
        return result['results']

    def coordinator(self):
        source=self.args.ato/'scripts/acceptance/proposal'
        deps=self.args.api/'node_modules'
        require(deps.is_dir(),'pinned API dependencies must be installed before execution')
        (self.receiver/'node_modules').symlink_to(deps,target_is_directory=True)
        shutil.copyfile(source/'coordinator.mjs',self.receiver/'coordinator.mjs')
        proc=self.start(['node',source/'build-receiver.mjs',self.args.api,self.receiver/'bundle',self.manifest['api_sha']],
                        self.args.run/'receiver-build.log')
        require(proc.wait(timeout=120)==0,'local receiver build failed')
        proc=self.start(['node','coordinator.mjs'],self.args.run/'coordinator.log',cwd=self.receiver)
        self.wait(lambda: (self.receiver/'commands').is_dir() if proc.poll() is None else False,'Coordinator initialization')

    def configure(self,cell,config):
        folder=self.args.run/'cells'/cell['id'];folder.mkdir(parents=True)
        token=folder/'coordinator-token'
        with token.open('x') as stream:stream.write('ato_rnr_'+secrets.token_hex(32))
        token.chmod(0o600)
        # Only a newly generated, isolated local Coordinator token. Not DeepSeek.
        owner='d3_'+cell['id']+'_'+secrets.token_hex(8)
        rid='rnr_'+owner
        self.sql('INSERT INTO "user"(id,name,email) VALUES(?,?,?)',[owner,owner,owner+'@acceptance.invalid'])
        self.sql('INSERT INTO runner_devices(id,user_id,display_name,token_hash) VALUES(?,?,?,?)',
                 [rid,owner,cell['id'],hashlib.sha256(token.read_bytes()).hexdigest()])
        config=dict(config,preregister_only=False,token_file=str(token),work=str(folder/'requester-work'),
                    created=str(folder/'created.json'),result=str(folder/'result.json'),calls=str(folder/'fixed-calls.jsonl'))
        transport=approved_plan(self.args.plan)['transport']
        config['live_llm']={'config':{'provider':transport['provider'],'model':transport['model'],
                          'endpoint':transport['endpoint'].removesuffix('/chat/completions'),
                          'prompt_version':'ato.formation-candidate-producer-prompt/1','max_output_tokens':transport['max_tokens'],
                          'timeout_ms':transport['timeout_ms'],'thinking':transport['thinking']},
                          'budget':approved_plan(self.args.plan)['budget'],'journal':str(self.args.journal)}
        config['expected_preregistration']=cell['projection']
        return folder,token,rid,config

    def requester(self,config,folder,index):
        path=folder/f'requester-{index}.json';write_new(path,config)
        return self.start([self.manifest['binaries']['requester']['path'],path],folder/f'requester-{index}.log',requester=True)


def recover_after_completion_loss(local,proxy,proc,config,folder):
    require(proxy.dropped.wait(60),'G5 completion drop not observed')
    local.stop(proc)
    require(proxy.recovery_only and proxy.saved_round is not None,'G5 lost response without confirmed commit')
    created=load(pathlib.Path(config['created']))
    resume=dict(config,resume=created['created']['satisfy_id'])
    # Same journal/source/pins, with proxy still rejecting ANY second claim or completion.
    return local.requester(resume,folder,1)


def execute(args,plan,configs,manifest):
    require(os.uname().sysname=='Linux' and os.uname().machine in ('aarch64','arm64'),'registered Runtime platform required')
    local=LocalRun(args,manifest)
    completed=[];results=[]
    try:
        local.coordinator()
        for index,cell_id in enumerate(plan['cell_order']):
            cell=next(c for c in plan['cells'] if c['id']==cell_id)
            require(not (args.run/'STOP.json').exists(),'run already stopped')
            approved_plan(args.plan);verify_binaries(manifest)
            helper(manifest['binaries']['budget']['path'],'reopen',args.plan,args.journal)
            journal_state(args.journal,plan,completed)
            folder,token,rid,config=local.configure(cell,configs[cell_id])
            proxy=None;runtime=None
            def capture(status,failure):
                status_path=folder/'preclaim-status.json'
                config_path=folder/'preclaim-config.json'
                out=folder/'preclaim-view.json'
                write_new(status_path,status);write_new(config_path,config)
                try:
                    subprocess.run([manifest['binaries']['project']['path'],str(config_path),str(status_path),str(out)],
                                   check=True,capture_output=True,timeout=15,env=local.environment)
                finally:
                    status_path.unlink(missing_ok=True)
                    config_path.unlink(missing_ok=True)
                evidence=load(out)
                require(evidence['projection_matches_registered'] is True,'preclaim source/K drift')
                if cell_id=='G2':
                    require(failure and evidence['failure_evidence'],'G2 actual projected failure absent')
                    write_new(folder/'known-failure.json',failure)
                # This hook intentionally cannot call the independently reproduced
                # preclaim hash an exact wire hash. CLI blocks before starting run.
                require(evidence['provider_wire_sha256'] is not None,'exact post-claim request evidence unavailable')
            try:
                proxy=create_proxy(cell,local.port,capture)
                config['api']=f'http://127.0.0.1:{proxy.server_port}'
                runtime=local.start([manifest['binaries']['runtime']['path'],f'http://127.0.0.1:{local.port}',token,
                                     folder/'runtime-work',folder/'runtime-out',manifest['binaries']['worker']['path']],folder/'runtime.log')
                local.wait(lambda:local.sql('SELECT runtime_id FROM runtime_environments WHERE runtime_id=?',[rid]),'Runtime advertisement')
                proc=local.requester(config,folder,0)
                if cell_id=='G5':proc=recover_after_completion_loss(local,proxy,proc,config,folder)
                require(proc.wait(timeout=180)==0 and proxy.stop_error is None,'requester/proxy infrastructure failure')
                result=load(pathlib.Path(config['result']))
                completed.append(config['search_id'])
                responses=journal_state(args.journal,plan,completed)
                outcome=cell_result(cell,result,responses,proxy.dropped.is_set())
                if cell_id=='G5':
                    require(proxy.recovery_gets>0 and result['status']['proposal_round']==proxy.saved_round,'G5 durable raw changed')
                outcome['reservation_index']=index+1
                request_evidence=load(folder/'preclaim-view.json')
                require(request_evidence['provider_wire_sha256'] is not None, 'exact request hash missing')
                outcome['request_sha256']=request_evidence['provider_wire_sha256']
                outcome['failure_evidence']=request_evidence['failure_evidence']
                outcome['inspection_evidence']=request_evidence['inspection_evidence']
                outcome['source_context_sha256']=request_evidence['source_context_sha256']
                write_new(folder/'cell.json',outcome)
                results.append(outcome)
            finally:
                if runtime:local.stop(runtime)
                if proxy:proxy.shutdown();proxy.server_close()
                token.unlink(missing_ok=True)
        # Only generated by an actually executed run, never by offline tests.
        write_new(args.run/'formation-deepseek-d3-results.json',dict(results=results,reserved_maximum_usd_micros=plan['reservation']['total_usd_micros'],
                  observed_token_usage=[r['provider_call']['provenance']['usage'] for r in results],
                  peak_price_equivalent_estimates_usd_micros=[r['provider_call']['provenance'].get('estimated_cost_usd_micros') for r in results],
                  actual_account_billing=None))
        summary=['# D3 live results','', 'Actual account billing: not measured.',
                 'Amounts below are peak-price-equivalent estimates, not actual account spend.',
                 f"Reserved maximum: {plan['reservation']['total_usd_micros']} USD micros.",'',
                 '| Cell | Gate | ContractRef | Peak-equivalent USD micros |', '|---|---|---|---:|']
        for result in results:
            cost=result['provider_call']['provenance'].get('estimated_cost_usd_micros')
            summary.append(f"| {result['cell']} | {result['gate']} | {result['contract_ref']} | {cost} |")
        with (args.run/'formation-deepseek-d3-results.md').open('x') as stream:
            stream.write('\n'.join(summary)+'\n')
    except Exception:
        if not (args.run/'STOP.json').exists():write_new(args.run/'STOP.json',dict(reason='protocol_or_infrastructure',next_call_allowed=False))
        raise
    finally:local.close()
