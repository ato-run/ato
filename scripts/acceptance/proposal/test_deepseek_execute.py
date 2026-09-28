"""D3-E offline E0–E19. Synthetic transport is Coordinator-only, never DeepSeek."""
import copy
import http.client
import http.server
import json
import pathlib
import sys
import tempfile
import threading
import types
import unittest
from datetime import datetime,timezone,timedelta
from unittest.mock import patch,Mock
sys.dont_write_bytecode=True
sys.path.insert(0,str(pathlib.Path(__file__).resolve().parent))
from execution import preflight as p
from execution import gates
from execution.proxy import create_proxy
from execution.runner import recover_after_completion_loss

ROOT=pathlib.Path(__file__).resolve().parents[3]
PLAN_PATH=ROOT/'docs/ops/formation-deepseek-d3-plan-v2.json'
PLAN=p.approved_plan(PLAN_PATH)


def status(cell):
    frozen=cell['projection']['frozen']
    return dict(contract_ref=frozen['contract_ref'],search_state=dict(frozen=frozen,attempts=[],revision=1,owner_stopped=False),
                attempts=[],proposal_point=dict(revision=1,round_seq=1,claimed=False,expires_at=(datetime.now(timezone.utc)+timedelta(seconds=30)).isoformat()),
                proposal_round=dict(status='open',expires_at_ms=int(datetime.now(timezone.utc).timestamp()*1000)+30000),status='running')


def failed_known(cell):
    value=status(cell);known=cell['projection']['frozen']['candidates'][0]['derivation_ref'];k=value['contract_ref']
    summary=dict(attempt_id='actual-test-attempt',derivation_ref=known,status='fail',record='finished',claimed=True,failure_code='http_status_mismatch')
    receipt=dict(schema='ato.contract-verification-receipt/1',contract_ref=k,derivation_ref=known,
                 execution=dict(attempt_id=summary['attempt_id'],realization='process'),fully_satisfied=False,
                 observations=[dict(outcome='failed',evidence=dict(status=404))])
    attestation=dict(execution_started=True,attempt_record='finished',contract_ref=k,derivation_ref=known)
    actual=dict(attempt_id=summary['attempt_id'],contract_ref=k,derivation_ref=known,failure=dict(stage='verification'),receipt=receipt)
    attempt=dict(attempt_id=summary['attempt_id'],derivation_ref=known,status='fail',finished_at='2026-09-28T00:00:00Z',
                 attestation=attestation,formation_attempt=actual)
    value['search_state']['attempts']=[summary];value['attempts']=[attempt]
    return value


class Offline(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(dir=ROOT/'.tmp')
        self.root=pathlib.Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_E0_wrong_plan_no_journal_no_call(self):
        wrong=self.root/'plan';wrong.write_text('{}')
        args=types.SimpleNamespace(plan=wrong,journal=self.root/'journal')
        with patch.object(p,'pin',side_effect=AssertionError('later barrier reached')):
            with self.assertRaisesRegex(p.Stop,'plan SHA'):p.check(args)
        self.assertFalse(args.journal.exists())

    def test_E1_wrong_ato_SHA(self):
        with patch.object(p,'git',return_value='wrong'):
            with self.assertRaisesRegex(p.Stop,'wrong checkout SHA'):p.pin(ROOT,p.ATO_SHA)

    def test_E2_dirty_checkout(self):
        with patch.object(p,'git',side_effect=[p.ATO_SHA,' M source.rs']):
            with self.assertRaisesRegex(p.Stop,'dirty checkout'):p.pin(ROOT,p.ATO_SHA)

    def test_E3_wrong_API_SHA(self):
        with patch.object(p,'git',return_value='wrong'):
            with self.assertRaisesRegex(p.Stop,'wrong checkout SHA'):p.pin(ROOT,p.API_SHA)

    def world(self,case):
        """Drive the real ordered preflight, replacing only external IO boundaries."""
        plan=copy.deepcopy(PLAN)
        ato=self.root/'ato';api=self.root/'api';run=self.root/'run'
        run.mkdir();ato.mkdir();api.mkdir()
        wasm=api/'src/services/runtime_network/wasm/receipt_authority.wasm';wasm.parent.mkdir(parents=True);wasm.write_bytes(b'wasm')
        prompt=ato/plan['prompt']['path'];prompt.parent.mkdir(parents=True);prompt.write_bytes(b'prompt')
        plan['prompt']['sha256']=p.digest(b'prompt')
        manifest=self.root/'binaries';manifest.write_text(json.dumps(dict(binaries={'requester':{'path':'not-run'},'budget':{'path':'not-run'}})))
        args=types.SimpleNamespace(plan=PLAN_PATH,ato=ato,api=api,run=run,journal=self.root/'journal',binaries=manifest)
        projections={}
        def prepare(root,cell,folder):
            target=folder/cell;target.mkdir(parents=True)
            result=target/'projection.json'
            saved=next(c for c in plan['cells'] if c['id']==cell)
            view=copy.deepcopy(saved['projection']);projections[cell]=view
            result.write_text(json.dumps(view))
            return dict(result=str(result),cell=cell),saved['files']
        def project(binary,cfg,folder):
            value=copy.deepcopy(projections[cfg['cell']])
            if case=='context' and cfg['cell']=='G0':
                saved=copy.deepcopy(value);saved['source_context_sha256']='wrong'
                pathlib.Path(cfg['result']).write_text(json.dumps(saved))
            return value
        def files(root,cell):
            value=next(c['files'] for c in plan['cells'] if c['id']==cell)
            return {} if case=='fixture' else value
        mod=p.prereg_module(ROOT)
        module=types.SimpleNamespace(fixture_files=files,prepare=prepare,project=project,sha=mod.sha,json_bytes=mod.json_bytes)
        def parse_price(page,checked):
            return dict(model='deepseek-flash',model_version='DeepSeek-V4.1-Flash',
                        input_price_usd_micros_per_million=300001 if case=='price' else 300000,
                        output_price_usd_micros_per_million=1200000)
        module.pricing_snapshot=parse_price
        def fetch(folder):
            page=folder/'price';page.write_bytes(b'public-html')
            now=datetime.now(timezone.utc)-timedelta(hours=2 if case=='stale' else 0)
            return page,dict(fetch_timestamp_utc=now.isoformat(),final_url=p.PRICE_URL,response_sha256=p.digest(page.read_bytes()))
        if case=='prompt':prompt.write_bytes(b'changed')
        if case=='journal':args.journal.write_bytes(b'corrupt\n')
        if case=='order':plan['cell_order']=list(reversed(plan['cell_order']))
        with patch.object(p,'approved_plan',return_value=plan),patch.object(p,'pin'),patch.object(p,'evidence_contract'),patch.object(p,'prereg_module',return_value=module),\
             patch.object(p,'WASM_SHA',p.digest(b'wrong' if case=='wasm' else b'wasm')),\
             patch.object(p,'helper',return_value=dict(per_call=81102,total=486612)),\
             patch.object(p,'verify_binaries'):
            return p.check(args,fetch=fetch)

    def test_E4_wrong_WASM(self):
        with self.assertRaisesRegex(p.Stop,'wrong WASM'):self.world('wasm')

    def test_E5_prompt_drift(self):
        with self.assertRaisesRegex(p.Stop,'prompt drift'):self.world('prompt')

    def test_E6_fixture_drift(self):
        with self.assertRaisesRegex(p.Stop,'fixture drift'):self.world('fixture')

    def test_E7_context_drift(self):
        with self.assertRaisesRegex(p.Stop,'source context drift'):self.world('context')

    def test_E8_price_increase(self):
        with self.assertRaisesRegex(p.Stop,'price increase'):self.world('price')

    def test_E9_stale_price(self):
        with self.assertRaisesRegex(p.Stop,'stale price'):self.world('stale')

    def test_E10_existing_corrupt_journal(self):
        with self.assertRaisesRegex(p.Stop,'existing/corrupt journal'):self.world('journal')
        self.assertEqual((self.root/'journal').read_bytes(),b'corrupt\n')

    def test_E11_wrong_cell_order(self):
        with self.assertRaisesRegex(p.Stop,'cell order'):self.world('order')

    def test_E12_G2_requires_actual_finished_same_K_known_failure(self):
        cell=PLAN['cells'][2];good=failed_known(cell)
        self.assertEqual(gates.known_failure(cell,good)['derivation_ref'],cell['projection']['frozen']['candidates'][0]['derivation_ref'])
        for mode in ['missing','unknown','running','wrong_D','wrong_K','no_execution','no_receipt','not_finished','fake_failure']:
            v=copy.deepcopy(good)
            if mode=='missing':v['attempts']=[]
            if mode in ('unknown','running'):v['search_state']['attempts'][0]['status']=mode
            if mode=='wrong_D':v['attempts'][0]['derivation_ref']='wrong'
            if mode=='wrong_K':v['attempts'][0]['formation_attempt']['receipt']['contract_ref']='wrong'
            if mode=='no_execution':v['attempts'][0]['attestation']['execution_started']=False
            if mode=='no_receipt':v['attempts'][0]['formation_attempt']['receipt']['observations']=[]
            if mode=='not_finished':v['attempts'][0]['finished_at']=None
            if mode=='fake_failure':v['attempts'][0]['formation_attempt']['failure']['stage']='launch'
            with self.subTest(mode=mode),self.assertRaises(p.Stop):gates.known_failure(cell,v)

    def test_E13_completion_loss_restart_GET_only(self):
        cell=PLAN['cells'][5]
        initial=status(cell);durable=copy.deepcopy(initial);durable['proposal_round']={'status':'completed','raw_output_base64':'e30='}
        class Coordinator(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def reply(self,value):
                data=json.dumps(value).encode();self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
            def do_GET(self):self.reply(durable if self.server.committed else initial)
            def do_PUT(self):
                data=self.rfile.read(int(self.headers.get('content-length','0')))
                self.reply({'uploaded':len(data)})
            def do_POST(self):
                self.rfile.read(int(self.headers.get('content-length','0')))
                if self.path.endswith('/claim'):
                    self.server.claims+=1;self.reply(dict(revision=2,round_seq=1,fence='00000000-0000-4000-8000-000000000001'))
                else:self.server.committed=True;self.reply({'revision':3})
        upstream=http.server.ThreadingHTTPServer(('127.0.0.1',0),Coordinator)
        upstream.committed=False;upstream.claims=0
        threading.Thread(target=upstream.serve_forever,daemon=True).start()
        captured=[];proxy=create_proxy(cell,upstream.server_port,lambda state,failure:captured.append(state))
        def request(method,path,body=None):
            conn=http.client.HTTPConnection('127.0.0.1',proxy.server_port,timeout=3)
            try:
                data=json.dumps(body).encode() if body else None
                conn.request(method,'/v1/runtime-network/satisfy/id'+path,data)
                response=conn.getresponse();return response.status,response.read()
            finally:conn.close()
        try:
            self.assertEqual(request('PUT','/source/content',{'archive':'synthetic'})[0],200)
            self.assertEqual(request('GET','')[0],200)
            self.assertEqual(request('POST','/proposal/claim',{'revision':1})[0],200)
            with self.assertRaises(http.client.RemoteDisconnected):request('POST','/proposal/complete',{})
            self.assertTrue(proxy.dropped.is_set());self.assertTrue(upstream.committed)
            created=self.root/'created';created.write_text(json.dumps({'created':{'satisfy_id':'id'}}))
            local=Mock();proc=Mock();local.requester.return_value='restarted'
            cfg=dict(created=str(created),live_llm=dict(journal='same-journal'))
            self.assertEqual(recover_after_completion_loss(local,proxy,proc,cfg,self.root),'restarted')
            resumed=local.requester.call_args.args[0]
            self.assertEqual(resumed['live_llm']['journal'],'same-journal');self.assertEqual(resumed['resume'],'id')
            self.assertEqual(request('GET','')[0],200);self.assertEqual(proxy.recovery_gets,1)
            self.assertEqual(request('POST','/proposal/claim',{'revision':3})[0],409)
            self.assertEqual(upstream.claims,1)
        finally:
            proxy.shutdown();proxy.server_close();upstream.shutdown();upstream.server_close()

    def test_E14_STOP_prevents_next_cell_and_never_resets(self):
        journal=self.root/'journal'
        with patch.object(gates,'helper',return_value={'stopped':True,'cells':{}}):
            with self.assertRaisesRegex(p.Stop,'run STOP'):
                gates.journal_state('not-run',PLAN_PATH,journal,[])
        self.assertFalse(journal.exists())

    def request_fixture(self):
        now=int(datetime.now(timezone.utc).timestamp()*1000)
        window={'expires_at_ms':now+28000,'claim_delivery_not_before_ms':now-5}
        saved={'cell':'G0','proposal_request_sha256':'sha256:'+'a'*64,
               'provider_body_sha256':'sha256:'+'b'*64,'timeout_ms':27500,
               'proposal_request_bytes':1000,'provider_body_bytes':2000,'response_resolved':True}
        return saved,window

    def inspect(self,saved,window,expected=None):
        with patch.object(gates,'helper',return_value=saved):
            return gates.request_evidence('not-run',PLAN_PATH,self.root/'journal','G0',window,expected)

    def test_E15_absent_request_STOP_no_live_send(self):
        _,window=self.request_fixture()
        with self.assertRaisesRegex(p.Stop,'RequestEvidence absent'):
            self.inspect({},window)
        self.assertFalse((self.root/'journal').exists())

    def test_E16_duplicate_request_parser_error_STOP(self):
        _,window=self.request_fixture()
        import subprocess
        with patch.object(gates,'helper',side_effect=subprocess.CalledProcessError(1,'Rust inspect-request')):
            with self.assertRaises(subprocess.CalledProcessError):
                gates.request_evidence('not-run',PLAN_PATH,self.root/'journal','G0',window)

    def test_E17_journal_controller_hash_mismatch_STOP(self):
        saved,window=self.request_fixture()
        for field in ('proposal_request_sha256','provider_body_sha256'):
            expected=copy.deepcopy(saved);expected[field]='sha256:'+'c'*64
            with self.subTest(field=field),self.assertRaisesRegex(p.Stop,'evidence mismatch'):
                self.inspect(saved,window,expected)

    def test_E18_timeout_formula_and_bounds(self):
        saved,window=self.request_fixture()
        self.assertEqual(self.inspect(saved,window),saved)
        for timeout in (0,30001,29500,1,True):
            changed={**saved,'timeout_ms':timeout}
            with self.subTest(timeout=timeout),self.assertRaises(p.Stop):
                self.inspect(changed,window)

    def test_E19_restart_request_event_increase_STOP(self):
        before={'cells':{'G5':{'request':self.request_fixture()[0],'response':{}}},'stopped':False}
        gates.unchanged_after_restart(before,copy.deepcopy(before))
        after=copy.deepcopy(before);after['cells']['second']={}
        with self.assertRaisesRegex(p.Stop,'restart changed'):
            gates.unchanged_after_restart(before,after)

    def test_ordered_A_through_M_offline(self):
        result=self.world('valid')
        self.assertEqual(result[3]['trace'],list('ABCDEFGHIJKLM'))
        self.assertTrue(result[3]['execution_ready'])

    def test_no_key_inspection_or_model_transport(self):
        for file in (ROOT/'scripts/acceptance/proposal/execution').glob('*.py'):
            text=file.read_text()
            for forbidden in ['os.environ','os.getenv','printenv','api.deepseek.com/chat/completions']:
                self.assertNotIn(forbidden,text)

if __name__=='__main__':unittest.main()
