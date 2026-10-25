#!/usr/bin/env python3
"""5c actual isolated Coordinator/D1/Runtime/Verifier; fixed providers only.
Reuses the C2 harness and production requester. No model or deployment client.
"""
import json,time,io,http.server,threading
import accept as a


class InvalidDecisionProxy(a.LossProxy):
    def forward(self):
        original = self.rfile
        try:
            if self.path.endswith('/decisions') and not self.server.injected:
                data=json.loads(self.rfile.read(int(self.headers.get('content-length','0'))))
                if self.server.mode == 'malformed':
                    body=b'{"seq":'
                elif self.server.mode in ['invalid', 'provider_error']:
                    body=json.dumps({'seq':data['seq'],'fallback':self.server.mode}).encode()
                else:
                    data['choice_id']='c0000000000000000' if self.server.mode == 'out_of_set' else 'not_an_offered_choice'
                    body=json.dumps(data).encode()
                self.headers.replace_header('Content-Length',str(len(body)))
                self.rfile=io.BytesIO(body);self.server.injected=True
            super().forward()
        finally:self.rfile=original
    do_GET=do_POST=do_PUT=forward

def escalation_rows(c):
    return a.sql('SELECT * FROM satisfy_decisions WHERE search_id=?',c['config']['search_id'])

def run(case):
    rt=None;proxy=None
    try:
        if case == 'A0':
            c=a.setup(case,proposals=[a.member('good')],prefer=0)
            rt=a.runtime(c);a.requester(c);r=a.finished(c);a.assert_pass(c,r,1)
            assert not r['decisions'] and not escalation_rows(c)
        elif case == 'A1_A2_A3_A9':
            c=a.setup(case,known=True,decision=True,prefer=0)
            rt=a.runtime(c);a.requester(c);r=a.finished(c);a.assert_pass(c,r,3)
            assert [x['status'] for x in r['status']['attempts']]==['fail','fail','pass']
            choices=[d for d in r['decisions'] if d.get('escalated')]
            assert len(choices)==1 and choices[0]['failure_evidence']
            assert len(a.sql('SELECT * FROM satisfy_proposal_rounds WHERE search_id=?',c['config']['search_id']))==1
            frozen=r['status']['search_state']['frozen']
            assert frozen['policy']['runtime_constraint']==c['frozen']['runtime_constraint']
        elif case.startswith('A8_'):
            c=a.setup(case,decision=True,proposals=[a.member('good')],prefer=0)
            proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),InvalidDecisionProxy)
            proxy.lock=threading.Lock();proxy.event=threading.Event();proxy.dropped=False
            proxy.suffix='/never-drop';proxy.injected=False;proxy.mode=case.removeprefix('A8_')
            threading.Thread(target=proxy.serve_forever,daemon=True).start()
            c['config']['api']=f'http://127.0.0.1:{proxy.server_port}'
            rt=a.runtime(c);a.requester(c);r=a.finished(c);a.assert_pass(c,r,1)
            assert proxy.injected
            rows=escalation_rows(c)
            expected={'out_of_set':'out_of_set','malformed':'timeout','wire_invalid':'timeout','invalid':'invalid','provider_error':'provider_error'}[proxy.mode]
            assert rows[0]['outcome']==expected,rows
            assert rows[0]['attempt_seq']==0 and rows[1]['seq']==1 and rows[1]['attempt_seq']==0
            assert rows[1]['outcome']=='chosen' and len(rows)==2
            assert rows[0]['chosen_choice'] is None
            rounds=a.sql('SELECT round_seq,status FROM satisfy_proposal_rounds WHERE search_id=?',c['config']['search_id'])
            assert rounds==[{'round_seq':1,'status':'completed'}],rounds
            assert len(r['status']['proposal_round']['outcomes'])==1
            assert r['status']['proposal_round']['outcomes'][0]['status']=='admitted'
            (c['root']/'decision-release.json').write_text(json.dumps({'decisions':rows,'rounds':rounds,'fixed_calls':a.count(c),'attempts_before_next_decision':rows[1]['attempt_seq']},indent=2))
        elif case == 'A6_deadline':
            c=a.setup(case,known=True,decision=True)
            c['config']['budget']['deadline_seconds']=1
            a.requester(c);r=a.finished(c)
            assert a.count(c)==0 and not r['status']['attempts']
            assert not escalation_rows(c) and r['status'].get('proposal_round') is None
            assert not r['accepted'];a.record(c,r)
        elif case == 'A6':
            c=a.setup(case,known=True,decision=True)
            c['config']['budget']['max_attempts']=1
            rt=a.runtime(c);a.requester(c);r=a.finished(c)
            assert a.count(c)==0 and len(r['status']['attempts'])==1
            assert not any('escalate_to_candidate_producer' in d['choices_json'] for d in escalation_rows(c)) and r['status'].get('proposal_round') is None
            assert not r['accepted'];a.record(c,r)
        elif case == 'A5':
            c=a.setup(case,known=True,decision=True)
            rt=a.runtime(c)
            a.sql("CREATE TRIGGER adaptive_pause_next_decision BEFORE INSERT ON satisfy_decisions WHEN NEW.search_id='"+c['config']['search_id']+"' AND NEW.seq>0 BEGIN SELECT RAISE(IGNORE); END")
            p=a.requester(c)
            before=a.wait(lambda:(lambda rows:rows if rows and rows[0]['status']=='fail' else None)(a.sql('SELECT * FROM satisfy_attempts WHERE satisfy_id=?',c['id'])),'actual known FAIL')
            a.stop(p)
            # Controlled durable-history fault, not fabricated K/Runtime evidence.
            (c['root']/'pre-fault-attempt.json').write_text(json.dumps(before[0]))
            a.sql("UPDATE satisfy_attempts SET result_json=json_set(result_json,'$.attestation.attempt_record','history_unavailable') WHERE id=?",before[0]['id'])
            a.sql('DROP TRIGGER adaptive_pause_next_decision')
            v=a.view(c)
            assert a.count(c)==0 and v.get('proposal_round') is None
            assert not any('escalate_to_candidate_producer' in d['choices_json'] for d in escalation_rows(c))
            assert 'effect_unknown' in json.dumps(v)
            r=json.loads((c['root']/'result.json').read_text());r['status']=v;(c['root']/'result.json').write_text(json.dumps(r));a.record(c,r)
        elif case == 'A4':
            c=a.setup(case,known=True,decision=True)
            rt=a.runtime(c,True);p=a.requester(c)
            v=a.wait(lambda:(lambda v:v if v['status']=='unknown' else None)(a.view(c)),'actual UNKNOWN',120)
            time.sleep(1);a.stop(p)
            assert a.count(c)==0 and v.get('proposal_round') is None
            assert not any('escalate_to_candidate_producer' in d['choices_json'] for d in escalation_rows(c))
            a.record(c,json.loads((c['root']/'result.json').read_text()))
        elif case == 'A7':
            c=a.setup(case,known=True,decision=True,prefer=0)
            rt=a.runtime(c)
            a.sql("CREATE TRIGGER adaptive_pause_proposal BEFORE INSERT ON satisfy_proposal_rounds WHEN NEW.search_id='"+c['config']['search_id']+"' BEGIN SELECT RAISE(IGNORE); END")
            p=a.requester(c)
            def settled():
                return [d for d in escalation_rows(c) if d['outcome']=='chosen' and 'escalate_to_candidate_producer' in d['choices_json']]
            before=a.wait(settled,'durable escalation before proposal opening')
            a.stop(p);assert a.count(c)==0
            assert not a.sql('SELECT * FROM satisfy_proposal_rounds WHERE search_id=?',c['config']['search_id'])
            a.sql('DROP TRIGGER adaptive_pause_proposal')
            p=a.requester(c,True);r=a.finished(c,p);a.assert_pass(c,r,3)
            after=settled();assert len(before)==len(after)==1
            assert before[0]['chosen_choice']==after[0]['chosen_choice']
        else: raise ValueError(case)
    finally:
        if rt:a.stop(rt)
        if proxy:proxy.shutdown();proxy.server_close()

if __name__=='__main__':
    import sys
    try:
        a.start_coordinator()
        for case in (sys.argv[1:] or ['A0','A1_A2_A3_A9','A4','A5','A6','A6_deadline','A7','A8_wire_invalid','A8_out_of_set','A8_malformed','A8_invalid','A8_provider_error']):run(case)
        (a.OUT/'adaptive-summary.json').write_text(json.dumps({'receiver':json.loads((a.RECEIVER/'bundle/receiver-pin.json').read_text()),'results':a.SUMMARY,'model_calls':0},indent=2))
        a.note('all',result='PASS',run=str(a.OUT))
    finally:
        for p in reversed(a.PROCESSES):a.stop(p)
        for token in a.OUT.glob('*/token'):token.unlink(missing_ok=True)
