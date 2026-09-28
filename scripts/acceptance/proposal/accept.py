#!/usr/bin/env python3
"""Isolated C1-pinned local D1 + production Rust requester/Runtime acceptance.
Run on a Linux test host with the preregistered Python toolchain. No model calls,
Cloudflare remote operations, production services, or fabricated PASS evidence.
Config: C2_ROOT (receiver/, source/), C2_BIN (Rust target/debug), optional CASEs.
"""
import hashlib, http.client, http.server, json, os, pathlib, secrets, shutil, signal
import socket, subprocess, sys, threading, time, urllib.request, uuid
ROOT=pathlib.Path(os.environ['C2_ROOT']).resolve()
SRC=ROOT/'source'; RECEIVER=ROOT/'receiver'; BIN=pathlib.Path(os.environ['C2_BIN'])
API='http://127.0.0.1:19544'; RUN=str(time.time_ns()); OUT=ROOT/'runs'/RUN;OUT.mkdir(parents=True)
ENV=dict(os.environ,TMPDIR=str(ROOT/'scratch'),PATH='/opt/ato/toolchains/node/22.14.0/bin:'+os.environ['PATH'])
FIX=SRC/'apps/formation-worker/fixtures/proposal-http'
K={'schema':'ato.contract/1','requirements':[{'id':'health','verifier':'ato.contract.http@1','port':'app.http','method':'GET','path':'/health','status':200,'body_digest':'sha256:'+hashlib.sha256((FIX/'health').read_bytes()).hexdigest()}]}
coordinator=None; PROCESSES=[]; SUMMARY={}; LEDGER=OUT/'ledger.jsonl'
def note(case,**v):
    item=dict(case=case,at=time.time(),**v)
    with LEDGER.open('a') as f:f.write(json.dumps(item)+'\n')
    print(json.dumps(item),flush=True)
def wait(f,why,timeout=90):
    until=time.monotonic()+timeout
    while time.monotonic()<until:
        value=f()
        if value:return value
        time.sleep(.1)
    raise AssertionError('timeout: '+why)
def stop(p):
    if p and p.poll() is None:
        os.killpg(p.pid,signal.SIGTERM)
        try:p.wait(timeout=15)
        except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait()
def launch(args,log,cwd=None):
    p=subprocess.Popen(list(map(str,args)),cwd=cwd,env=ENV,stdout=open(log,'w'),stderr=subprocess.STDOUT,start_new_session=True)
    PROCESSES.append(p);return p
def start_coordinator():
    global coordinator
    log=OUT/f'coordinator-{time.time_ns()}.log'
    coordinator=launch(['node','coordinator.mjs'],log,RECEIVER)
    def ready():
        assert coordinator.poll() is None,log.read_text()[-6000:]
        return 'ready' in log.read_text()
    wait(ready,'Coordinator ready',120)
def sql(q,*params):
    folder=RECEIVER/'commands';name=str(time.time_ns())
    temp=folder/(name+'.tmp');temp.write_text(json.dumps({'sql':q,'params':params}));temp.rename(folder/(name+'.in.json'))
    out=folder/(name+'.out.json');wait(out.exists,'SQL result',45)
    result=json.loads(out.read_text());assert 'error' not in result,result
    return result['results']
def view(c):
    req=urllib.request.Request(API+'/v1/runtime-network/satisfy/'+c['id'],headers={'authorization':'Bearer '+c['token'].read_text().strip()})
    with urllib.request.urlopen(req,timeout=30) as r:return json.load(r)
def count(c):return len(c['calls'].read_text().splitlines()) if c['calls'].exists() else 0
def member(entry):return {'kind':'propose_derivation','operations':[{'operation':'python_http_process@1','entrypoint_id':entry}]}
def route(entry):
    digest=K['requirements'][0]['body_digest']
    return f'''schema = "ato.capsule/1"
[[input]]
id="workspace"
use="ato.workspace@1"
path="."
[[runtime]]
name="python"
version="3.12.7"
[[derive.step]]
id="app"
use="ato.process@1"
op="serve"
cwd="."
argv=["/opt/ato/toolchains/python/3.12.7/bin/python3","-B","/app/{entry}"]
[[port]]
id="app.http"
use="ato.http@1"
from="app"
guest_port=8000
[[contract.require]]
id="health"
use="ato.contract.http@1"
port="app.http"
method="GET"
path="/health"
[contract.require.expect]
status=200
body_digest="{digest}"
'''
def setup(case,known=False,enabled=True,mode='success',proposals=None,prefer=1,decision=False):
    root=OUT/case;root.mkdir();source=root/'source';shutil.copytree(FIX,source)
    # Separate source transport identity per test, never a provider input.
    (source/'case.txt').write_text(case+'\n');(source/'known_bad.py').write_bytes((source/'bad.py').read_bytes())
    token=root/'token';token.write_text('ato_rnr_'+secrets.token_hex(32));token.chmod(0o600)
    owner='u_'+case+'_'+RUN;rid='rnr_'+case+'_'+RUN
    sql('INSERT INTO "user"(id,name,email) VALUES(?,?,?)',owner,owner,owner+'@acceptance.invalid')
    sql('INSERT INTO runner_devices(id,user_id,display_name,token_hash) VALUES(?,?,?,?)',rid,owner,case,hashlib.sha256(token.read_bytes()).hexdigest())
    policy={'network':'denied','allow_managed':False}
    if decision:policy['decision']={'provider':'requester','max_decisions':8,'decision_timeout_ms':10000}
    config={'api':API,'token_file':str(token),'source':str(source),'work':str(root/'work'),
        'search_id':case+'_'+RUN,'claimant_id':str(uuid.uuid4()),'contract':K,'routes':[],
        'authorization':{'modifiable_derivation_refs':[],'source_domain':{'entrypoints':{'bad':'bad.py','good':'serve.py'},'modules':{}},
            'python_http_process':{'python_version':'3.12.7','http_port':'app.http','guest_port':8000},
            'policy':{'max_proposal_rounds':1,'max_proposals':4,'timeout_ms':10000,'allow_source_text':False,'max_source_bytes':0}} if enabled else None,
        'policy':policy,'budget':{'max_attempts':4,'mode':'first_pass','deadline_seconds':180,'max_transfer_bytes':536870912,'max_expanded_bytes':1073741824,'max_stored_bytes':1073741824},
        'batch_json':json.dumps({'schema':'ato.formation-proposal/1','proposals':proposals if proposals is not None else [member('bad'),member('good')]}),
        'producer_mode':mode,'calls':str(root/'calls.jsonl'),'created':str(root/'created.json'),'result':str(root/'result.json'),'prefer_member':prefer,'settle_seconds':150}
    if known:
        f=root/'known.toml';f.write_text(route('known_bad.py'));config['routes']=[str(f)]
    c=dict(root=root,config=config,token=token,calls=root/'calls.jsonl',owner=owner,rid=rid)
    return c
def requester(c,resume=False,extra=None):
    config=dict(c['config']);tag=str(time.time_ns())
    if resume:config['resume']=c['id']
    if extra:config.update(extra)
    path=c['root']/('config-'+tag+'.json');path.write_text(json.dumps(config));log=c['root']/('request-'+tag+'.log')
    p=launch([BIN/'examples/proposal_search',path],log)
    def created():
        assert p.poll() in (None,0,86),log.read_text()[-5000:]
        f=pathlib.Path(config['created'])
        if not f.exists() or not f.stat().st_size:return None
        try:return json.loads(f.read_text())
        except json.JSONDecodeError:return None
    data=wait(created,'request created',90);c['id']=data['created']['satisfy_id'];c['frozen']=data['request'];c['requester']=p
    return p
def runtime(c,unknown=False):
    root=c['root']/'runtime';root.mkdir(exist_ok=True)
    if unknown:
        records=root/'out/attempt-records';records.mkdir(parents=True);records.chmod(0o555)
    p=launch([BIN/'examples/proposal_runtime',API,c['token'],root/'work',root/'out',BIN/'ato-formation-worker'],root/'log')
    wait(lambda:sql('SELECT * FROM runtime_environments WHERE runtime_id=?',c['rid']),'Runtime advertisement',120)
    return p
def finished(c,p=None):
    p=p or c['requester'];rc=p.wait(timeout=180)
    logs=list(c['root'].glob('request-*.log'))
    assert rc==0,(rc,[f.read_text()[-4000:] for f in logs])
    result=json.loads((c['root']/'result.json').read_text());v=result['status']
    assert result['frozen_contract']==K
    assert v['search_state']['frozen']['base_contract']==K
    assert v['contract_ref']==c['frozen']['contract_ref']==result['contract_ref']
    for call in c['calls'].read_text().splitlines() if c['calls'].exists() else []:
        data=json.loads(call);request=json.dumps(data['request'])
        for private in ['serve.py','bad.py','known_bad.py','source_domain','capsule_toml','token','archive_digest']:
            assert private not in request,private
    return result
def assert_pass(c,result,attempts,calls=1):
    v=result['status'];assert v['status']=='satisfied',v
    assert len(v['attempts'])==attempts and count(c)==calls,(v['attempts'],count(c))
    assert result['accepted'] and not result['refused'] and result['receipt_required_checked']
    for route in v['verified_routes']:
        assert route['effective_contract_ref']==c['frozen']['contract_ref']
        receipts=[x['receipt'] for x in route['verifier_receipts'] if x['kind']=='contract_verification']
        assert len(receipts)==1
        receipt=receipts[0]
        assert receipt['fully_satisfied'] and receipt['contract_ref']==c['frozen']['contract_ref']
        assert receipt['derivation_ref']==route['derivation_ref']
        assert receipt['execution']['attempt_id']==route['attempt_id']
        assert receipt['execution']['realization']=='process'
        observation=receipt['observations'][0]
        assert observation['outcome']=='satisfied' and observation['evidence']['status']==200
        assert observation['evidence']['body_sha256']==K['requirements'][0]['body_digest']
    record(c,result)
def record(c,result):
    v=result['status'];summary={'status':v['status'],'calls':count(c),'contract_ref':result['contract_ref'],
        'known_count':len(v['search_state']['frozen']['candidates']),
        'attempts':[{'id':a['attempt_id'] if 'attempt_id' in a else a.get('id'),'d':a['derivation_ref'],'status':a['status']} for a in v['attempts']],
        'accepted_routes':len(result['accepted']),'proposal_status':(v.get('proposal_round') or {}).get('status'),
        'outcomes':[o['status'] for o in (v.get('proposal_round') or {}).get('outcomes',[])],
        'result_sha256':hashlib.sha256((c['root']/'result.json').read_bytes()).hexdigest()}
    SUMMARY[c['root'].name]=summary;note(c['root'].name,**summary)

class LossProxy(http.server.BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*args):pass
    def forward(self):
        if self.path.endswith('/proposal/claim') and getattr(self.server,'claim_barrier',None):
            self.server.claim_barrier.wait(timeout=20)
        length=int(self.headers.get('content-length','0'));body=self.rfile.read(length)
        if self.headers.get('transfer-encoding','').lower()=='chunked':
            body=b''
            while True:
                n=int(self.rfile.readline().split(b';')[0],16)
                if not n:self.rfile.readline();break
                body+=self.rfile.read(n);self.rfile.read(2)
        headers={k:v for k,v in self.headers.items() if k.lower() not in ['host','connection','transfer-encoding','content-length']}
        headers['content-length']=str(len(body));conn=http.client.HTTPConnection('127.0.0.1',19544,timeout=60)
        conn.request(self.command,self.path,body,headers);r=conn.getresponse();data=r.read();status=r.status;response_headers=r.getheaders();conn.close()
        with self.server.lock:
            lose=self.path.endswith(self.server.suffix) and status==200 and not self.server.dropped
            if lose:self.server.dropped=True;self.server.event.set()
        if lose:
            self.connection.shutdown(socket.SHUT_RDWR);self.connection.close();return
        self.send_response(status)
        for k,v in response_headers:
            if k.lower() not in ['content-length','connection','transfer-encoding']:self.send_header(k,v)
        self.send_header('Content-Length',str(len(data)));self.end_headers()
        try:self.wfile.write(data)
        except (BrokenPipeError,ConnectionResetError):
            # The loss/restart cases intentionally terminate the HTTP client.
            pass
    do_GET=do_POST=do_PUT=forward

def cases(case):
    rt=None;proxy=None
    try:
        if case=='zero':
            c=setup(case,proposals=[member('good')],prefer=0);rt=runtime(c);requester(c);r=finished(c);assert_pass(c,r,1)
            assert c['frozen']['authorized_derivations']==[] and not (c['root']/'source/capsule.toml').exists()
        elif case in ['P1_P2_P10_P11','P3','P5']:
            proposals=[member('bad'),member('good')]
            if case=='P5':proposals +=[member('good'),member('unauthorized')]
            c=setup(case,known=True,decision=True,prefer=0 if case=='P3' else 1,proposals=proposals)
            rt=runtime(c);requester(c);r=finished(c);assert_pass(c,r,3 if case=='P3' else 2)
            assert r['status']['attempts'][0]['status']=='fail'
            if case=='P3':
                bad=r['status']['proposal_round']['outcomes'][0]['derivation_ref']
                good=r['status']['proposal_round']['outcomes'][1]['derivation_ref']
                assert r['status']['attempts'][1]['status']=='fail'
                assert any(d['selected']==bad for d in r['decisions'])
                assert any(d['selected']==good and bad in d['failed_derivations'] and d['failure_evidence'] for d in r['decisions'])
            assert len(r['status']['proposal_round']['outcomes'])==len(proposals)
            if case=='P5':assert [x['status'] for x in r['status']['proposal_round']['outcomes']]==['admitted','admitted','rejected','rejected']
            # Compiler/choice alone never authorize a fabricated PASS receipt.
            assert r['status']['verified_routes'] and r['accepted']
        elif case=='P0':
            c=setup(case,known=True,enabled=False);rt=runtime(c);requester(c);r=finished(c)
            assert count(c)==0 and len(r['status']['attempts'])==1 and not r['accepted'];assert 'proposal_round' not in r['status'];record(c,r)
        elif case=='P4':
            bad=[{'kind':'propose_derivation','contract':{'requirements':[]},'operations':[]},
                {'kind':'propose_derivation','operations':[{'operation':'shell','argv':['sh']}]},member('unauthorized')]
            c=setup(case,proposals=bad);rt=runtime(c);requester(c);r=finished(c)
            assert count(c)==1 and not r['status']['attempts'] and not r['accepted'];assert all(o['status']=='rejected' for o in r['status']['proposal_round']['outcomes']);record(c,r)
        elif case in ['P6_error','P6_timeout','unsupported']:
            c=setup(case,mode={'P6_error':'error','P6_timeout':'timeout','unsupported':'success'}[case],proposals=[{'kind':'unsupported'}]);rt=runtime(c);requester(c);r=finished(c)
            assert count(c)==1 and not r['status']['attempts'] and not r['accepted'];record(c,r)
        elif case=='P7':
            c=setup(case,mode='crash');rt=runtime(c);p=requester(c);assert p.wait(timeout=45)==86
            p=requester(c,True,{'producer_mode':'success'});r=finished(c,p)
            assert count(c)==1 and not r['status']['attempts'] and r['status']['proposal_round']['status']=='timeout';record(c,r)
        elif case=='P8':
            c=setup(case,proposals=[member('good')],prefer=0)
            # Both real claim HTTP requests reach the barrier before either is
            # forwarded. This is a claim race, not two sequential restart GETs.
            proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),LossProxy)
            proxy.lock=threading.Lock();proxy.event=threading.Event();proxy.dropped=False
            proxy.suffix='/never-drop';proxy.claim_barrier=threading.Barrier(2)
            threading.Thread(target=proxy.serve_forever,daemon=True).start()
            c['config']['api']=f'http://127.0.0.1:{proxy.server_port}'
            rt=runtime(c)
            p=requester(c)
            p2=requester(c,True,{'work':str(c['root']/'work-2'),'claimant_id':str(uuid.uuid4()),'created':str(c['root']/'created-2.json'),'result':str(c['root']/'result-2.json')})
            r=finished(c,p);assert p2.wait(timeout=180)==0;assert_pass(c,r,1)
        elif case=='P9':
            c=setup(case,known=True);rt=runtime(c,True);p=requester(c)
            v=wait(lambda:(lambda v:v if v['status']=='unknown' else None)(view(c)),'actual UNKNOWN',120)
            time.sleep(1);stop(p);assert count(c)==0 and v.get('proposal_round') is None
            r=json.loads((c['root']/'result.json').read_text());assert not r['accepted'];record(c,r)
        elif case in ['L1','L2']:
            c=setup(case,proposals=[member('good')],prefer=0);rt=runtime(c)
            proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),LossProxy);proxy.lock=threading.Lock();proxy.event=threading.Event();proxy.dropped=False
            proxy.suffix='/proposal/claim' if case=='L1' else '/proposal/complete'
            threading.Thread(target=proxy.serve_forever,daemon=True).start();c['config']['api']=f'http://127.0.0.1:{proxy.server_port}'
            p=requester(c);assert proxy.event.wait(45),'response loss not exercised';time.sleep(.2);stop(p)
            p=requester(c,True,{'api':API});r=finished(c,p)
            if case=='L1':assert count(c)==0 and not r['status']['attempts'];record(c,r)
            else:assert_pass(c,r,1)
        elif case=='L3':
            c=setup(case,proposals=[member('good')],prefer=0)
            rt=runtime(c)
            # Pause only this owner's normal attempt insert, not compilation or
            # verification. Completion alone must not become a verified route.
            sql("CREATE TRIGGER c2_pause_issue BEFORE INSERT ON satisfy_attempts WHEN NEW.satisfy_id IN (SELECT id FROM satisfy_requests WHERE requested_by_user_id='"+c['owner']+"') BEGIN SELECT RAISE(IGNORE); END")
            p=requester(c);wait(lambda:(lambda v:v if (v.get('proposal_round') or {}).get('status')=='completed' else None)(view(c)),'durable completion')
            v=view(c);before=v['proposal_round'];assert not v['verified_routes'];stop(p);stop(coordinator);start_coordinator()
            assert view(c)['proposal_round']==before
            sql('DROP TRIGGER c2_pause_issue')
            p=requester(c,True);r=finished(c,p);assert_pass(c,r,1)
        else:raise ValueError(case)
    finally:
        if rt:stop(rt)
        if proxy:proxy.shutdown();proxy.server_close()
        for token in OUT.glob('*/token'):token.unlink(missing_ok=True)

if __name__=='__main__':
    requested=sys.argv[1:] or ['zero','P0','P1_P2_P10_P11','P3','P4','P5','P6_error','P6_timeout','unsupported','P7','P8','P9','L1','L2','L3']
    try:
        start_coordinator()
        assert sql("SELECT name FROM d1_migrations WHERE name='0304_formation_proposals.sql'")
        for case in requested:cases(case)
        (OUT/'summary.json').write_text(json.dumps({'receiver':json.loads((RECEIVER/'bundle/receiver-pin.json').read_text()),'results':SUMMARY,'model_calls':0},indent=2))
        note('all',result='PASS',run=str(OUT))
    finally:
        for p in reversed(PROCESSES):stop(p)
        for token in OUT.glob('*/token'):token.unlink(missing_ok=True)
