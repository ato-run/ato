"""Bounded local Coordinator/Runtime control. No model client or key reads.

Evidence is operational; only the pinned Rust requester invokes the model and
accepts Runtime/Verifier routes. Controller code never reconstructs wire bytes.
"""
import hashlib
import json
import os
import pathlib
import secrets
import shutil
import signal
import subprocess
import sys
import time
from .preflight import Stop, require, write_new, verify_binaries, approved_plan, helper, load, digest
from .gates import journal_state, cell_result, request_evidence, unchanged_after_restart
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
        # Never copy/enumerate the controller environment. All processes get this
        # non-secret map; only the requester injector receives the sealed fd.
        self.environment={'PATH':'/opt/ato/toolchains/node/22.14.0/bin:/opt/ato/toolchains/python/3.12.7/bin:/usr/local/bin:/usr/bin:/bin',
                          'HOME':str(self.home),'TMPDIR':str(self.scratch)}

    def start(self,argv,log,cwd=None,requester=False):
        verify_binaries(self.manifest)
        argv = [str(x) for x in argv]
        inherited_fds = ()
        environment = self.environment
        credential_fd = getattr(self.args,'credential_fd',None)
        if requester and credential_fd is not None:
            # Never read/check/hash the credential here, including its existence.
            # The separately authorized injector supplied sealed anonymous memory
            # after preflight. Only the requester wrapper inherits that descriptor.
            argv = [sys.executable,self.manifest['credential_wrapper']['path'],str(credential_fd),*argv]
            inherited_fds = (credential_fd,)
            environment = self.environment
        with log.open('xb') as stream:
            proc=subprocess.Popen(argv,cwd=cwd,env=environment,pass_fds=inherited_fds,
                                  stdin=subprocess.DEVNULL,stdout=stream,stderr=subprocess.STDOUT,start_new_session=True)
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


def execution_platform():
    require(os.uname().sysname=='Linux' and os.uname().machine in ('aarch64','arm64'),'registered Runtime platform required')


def execute(args,plan,configs,manifest):
    execution_platform()
    local=LocalRun(args,manifest)
    completed=[];results=[]
    cell_id=None;folder=None
    try:
        local.coordinator()
        for index,cell_id in enumerate(plan['cell_order']):
            cell=next(c for c in plan['cells'] if c['id']==cell_id)
            require(not (args.run/'STOP.json').exists(),'run already stopped')
            approved_plan(args.plan);verify_binaries(manifest)
            journal_state(manifest['binaries']['budget']['path'],args.plan,args.journal,completed)
            folder,token,rid,config=local.configure(cell,configs[cell_id])
            proxy=None;runtime=None
            captured_request=None
            before_restart=None
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
            def capture_completion(window):
                nonlocal captured_request
                captured_request=request_evidence(manifest['binaries']['budget']['path'],args.plan,
                    args.journal,config['search_id'],window,require_response=False)
                write_new(folder/'request-evidence.json',captured_request)
                write_new(folder/'claim-timing.json',window)
                snapshot=helper(manifest['binaries']['budget']['path'],'snapshot',args.plan,args.journal)
                return captured_request['response_resolved'] and not snapshot['stopped']
            try:
                proxy=create_proxy(cell,local.port,capture,capture_completion)
                config['api']=f'http://127.0.0.1:{proxy.server_port}'
                runtime=local.start([manifest['binaries']['runtime']['path'],f'http://127.0.0.1:{local.port}',token,
                                     folder/'runtime-work',folder/'runtime-out',manifest['binaries']['worker']['path']],folder/'runtime.log')
                local.wait(lambda:local.sql('SELECT runtime_id FROM runtime_environments WHERE runtime_id=?',[rid]),'Runtime advertisement')
                proc=local.requester(config,folder,0)
                if cell_id=='G5':
                    require(proxy.dropped.wait(60),'G5 completion drop not observed')
                    before_restart=helper(manifest['binaries']['budget']['path'],'snapshot',args.plan,args.journal)
                    proc=recover_after_completion_loss(local,proxy,proc,config,folder)
                require(proc.wait(timeout=180)==0 and proxy.stop_error is None,'requester/proxy infrastructure failure')
                result=load(pathlib.Path(config['result']))
                completed.append(config['search_id'])
                responses=journal_state(manifest['binaries']['budget']['path'],args.plan,args.journal,completed)
                outcome=cell_result(cell,result,responses,proxy.dropped.is_set())
                if cell_id=='G5':
                    require(proxy.recovery_gets>0 and result['status']['proposal_round']==proxy.saved_round,'G5 durable raw changed')
                outcome['reservation_index']=index+1
                require(captured_request is not None,'RequestEvidence absent')
                inspected=request_evidence(manifest['binaries']['budget']['path'],args.plan,
                    args.journal,config['search_id'],proxy.claim_window,captured_request)
                if cell_id=='G5':
                    unchanged_after_restart(before_restart,helper(manifest['binaries']['budget']['path'],'snapshot',args.plan,args.journal))
                outcome['request_evidence']=inspected
                for key in ('proposal_request_sha256','provider_body_sha256','proposal_request_bytes','provider_body_bytes'):
                    outcome[key]=inspected[key]
                outcome['actual_timeout_ms']=inspected['timeout_ms']
                preclaim=load(folder/'preclaim-view.json')
                for key in ('failure_evidence','inspection_evidence','source_context_sha256','known_derivations'):
                    outcome[key]=preclaim[key]
                write_new(folder/'cell.json',outcome)
                results.append(outcome)
            finally:
                if runtime:local.stop(runtime)
                if proxy:proxy.shutdown();proxy.server_close()
                token.unlink(missing_ok=True)
        persist_results(args,plan,results)
    except Exception:
        if not (args.run/'STOP.json').exists():write_new(args.run/'STOP.json',dict(reason='protocol_or_infrastructure',next_call_allowed=False))
        partial={'cell':cell_id,'terminal_classification':'protocol_or_infrastructure'}
        if folder is not None:
            request=folder/'request-evidence.json'
            if request.exists():partial['request_evidence']=load(request)
            result_path=folder/'result.json'
            if result_path.exists():
                round=load(result_path).get('status',{}).get('proposal_round') or {}
                partial['provider_call']=round.get('provider_call')
        persist_results(args,plan,results,stopped=True,partial_cell=partial)
        raise
    finally:local.close()

def persist_results(args,plan,results,stopped=False,partial_cell=None):
    # Never invoked by preflight/build. Do not call estimates actual billing.
    write_new(args.run/'formation-deepseek-d3-results.json',dict(results=results,stopped=stopped,partial_cell=partial_cell,reserved_maximum_usd_micros=plan['reservation']['total_usd_micros'],
              observed_token_usage=[r['provider_call']['provenance']['usage'] for r in results],
              peak_price_equivalent_estimates_usd_micros=[r['provider_call']['provenance'].get('estimated_cost_usd_micros') for r in results],
              actual_account_billing=None))
    summary=['# D3 live results','', f'Run stopped: {stopped}.', 'Actual account billing: not measured.',
             'Amounts below are peak-price-equivalent estimates, not actual account spend.',
             f"Reserved maximum: {plan['reservation']['total_usd_micros']} USD micros.",'',
             '| Cell | Gate | ContractRef | Peak-equivalent USD micros |', '|---|---|---|---:|']
    for result in results:
        cost=result['provider_call']['provenance'].get('estimated_cost_usd_micros')
        summary.append(f"| {result['cell']} | {result['gate']} | {result['contract_ref']} | {cost} |")
    with (args.run/'formation-deepseek-d3-results.md').open('x') as stream:
        stream.write('\n'.join(summary)+'\n')
