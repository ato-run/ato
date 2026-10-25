"""Post-evaluation fixture validity audit only; three fixed drafts, ZERO model calls.
Never changes E1 outcomes, denominators, rules or provider observations.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
from fixtures import materialize

ROOT=Path(__file__).resolve().parents[3]
HERE=ROOT/'scripts/acceptance'
spec=importlib.util.spec_from_file_location('oracle_context',HERE/'formation-generation-context.py')
c=importlib.util.module_from_spec(spec);sys.modules[spec.name]=c;spec.loader.exec_module(c)

def main():
    plan=json.loads((ROOT/'docs/ops/formation-efficacy-e1-amendment.json').read_text())
    c.load_helpers();h,d,v1=c.h,c.d,c.v1
    assert hashlib.sha256(h.REQ.read_bytes()).hexdigest()==plan['artifacts']['requester']
    out=h.S4/'efficacy-e1-oracle';out.mkdir(exist_ok=False)
    h.LEDGER.close();h.OUT=v1.OUT=c.OUT=out;h.LEDGER=(out/'ledger.jsonl').open('x')
    results=[];h.start_coordinator('oracle')
    try:
        for case in ['E04','E06','E07']:
            root=out/case;root.mkdir();d.OWNER=f'oracle{case}{v1.RUN_ID}';d.TOKEN=out/f'token-{case}';d.ensure_owner()
            h.OWNER,h.TOKEN=d.OWNER,d.TOKEN
            entries,hashes=materialize(h.FIX/'notes',root/'source',case,0)
            assert hashes==plan['fixtures'][case]['files']
            route=root/'base.toml';route.write_text((root/'source/capsule.toml').read_text().replace('/app/app.py','/app/bad.py'))
            run=c.Run(case,root,f'oracle{case}{v1.RUN_ID}',entries=entries,owner=d.OWNER,token=d.TOKEN)
            run.fixture_sha256=hashlib.sha256(json.dumps(hashes,sort_keys=True,separators=(',',':')).encode()).hexdigest()
            try:
                run.runtime=h.runtime(case,'runtime',2)
                rid=h.sql('SELECT id FROM runner_devices WHERE user_id=?',d.OWNER)[0]['id']
                h.wait_for(lambda:h.sql('SELECT environment_id FROM runtime_environments WHERE runtime_id=?',rid),'runtime',timeout=120)
                env={k:v for k,v in h.ENV.items() if not k.startswith('ATO_ACCEPTANCE_') and 'JEV' not in k and 'TYPESAFE' not in k}
                env.update(ATO_ACCEPTANCE_SETTLE_SECS='180',ATO_ACCEPTANCE_EXACT_RUNTIME=rid,
                    ATO_ACCEPTANCE_GENERATION_ENTRYPOINTS=json.dumps(entries),ATO_ACCEPTANCE_GENERATION_CONTEXT='v2',
                    ATO_ACCEPTANCE_GENERATION_PROVIDER='fixed:q7',ATO_ACCEPTANCE_GENERATION_LOG=str(root/'generation-calls.jsonl'),
                    ATO_ACCEPTANCE_GENERATION_INPUT=str(root/'provider-request.json'))
                with (root/'status.json').open('w') as stdout,(root/'request.log').open('w') as stderr:
                    run.requester=subprocess.Popen([str(h.REQ),h.API,str(d.TOKEN),str(root/'source'),str(root/'work'),run.sid,'2',str(root/'created.json'),str(route)],env=env,stdout=stdout,stderr=stderr,start_new_session=True)
                h.wait_for(lambda:(root/'created.json').exists() and (root/'created.json').read_text(),'created',timeout=120)
                run.satisfy=c.read_json(root/'created.json')['satisfy_id'];snapshot=c.settle(run)
                c.admitted(run,snapshot)
                results.append({'case':case,'contract_ref':snapshot['contract_ref'],'same_k_pass':True,'model_calls':0,
                    'fixed_provider_calls':snapshot['provider_call_count'],'fixture_hash':run.fixture_sha256,
                    'derivation_ref':snapshot['generated_derivation_ref'],'requester_exit':run.exit_code})
            finally:
                c.cleanup(run);d.TOKEN.unlink(missing_ok=True)
                (out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
    finally:h.stop_coordinator()

if __name__=='__main__':main()
