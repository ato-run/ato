#!/usr/bin/env python3
"""Exercise the actual Node wrapper and child processes, not application E2E.

Own fixtures live under .tmp. No Coordinator, inference, credentials, network,
deployment, application source rewrite or UNKNOWN replay is involved.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT=Path(__file__).resolve().parents[3]
SCRIPT=ROOT/'lib/formation/src/proposal/node-runtime-scripts.cjs'
NODE=shutil.which('node')
assert NODE,'Node must already be available'
(ROOT/'.tmp').mkdir(exist_ok=True)
results=[]
with tempfile.TemporaryDirectory(prefix='runtime-script-deadline-',dir=ROOT/'.tmp') as temporary:
    root=Path(temporary)
    manifest=root/'package.json'
    manifest.write_text(json.dumps({'scripts':{'setup':'owned setup fixture','start':'owned launch fixture'}}))
    fake_npm=root/'fixture-npm'
    fake_npm.write_text('#!'+NODE+'''\n'use strict';
const fs=require('node:fs');
const script=process.argv[3];
fs.appendFileSync(process.env.OWNED_LOG,JSON.stringify({script,
  private_variable_present:process.env.PRIVATE_VARIABLE==='owned fixture value',
  deadline_exposed:process.env.ATO_FORMATION_EXECUTION_DEADLINE_MS!==undefined,
  remaining_exposed:process.env.ATO_FORMATION_EXECUTION_REMAINING_MS!==undefined})+'\\n');
if(script==='setup')setTimeout(()=>{},Number(process.env.SETUP_DELAY_MS||0));
''')
    fake_npm.chmod(0o700)
    plan={'manifest':str(manifest),'manifest_sha256':'sha256:'+hashlib.sha256(manifest.read_bytes()).hexdigest(),
          'npm':str(fake_npm),'setup_scripts':['setup'],'launch_script':'start','argv':[]}
    fault=root/'clock-rollback.cjs'
    fault.write_text("const original=Date.now;setTimeout(()=>{Date.now=()=>original()-100000;},20);\n")
    def run(name,deadline=None,remaining=None,delay=0,rollback=False):
        log=root/(name+'.jsonl')
        env={k:v for k,v in os.environ.items() if k not in ('ATO_FORMATION_EXECUTION_DEADLINE_MS','ATO_FORMATION_EXECUTION_REMAINING_MS','NODE_OPTIONS')}
        env.update({'OWNED_LOG':str(log),'PRIVATE_VARIABLE':'owned fixture value','SETUP_DELAY_MS':str(delay),
                    'TMPDIR':str(root)})
        if deadline is not None:env['ATO_FORMATION_EXECUTION_DEADLINE_MS']=str(deadline)
        if remaining is not None:env['ATO_FORMATION_EXECUTION_REMAINING_MS']=str(remaining)
        command=[NODE]+(['--require',str(fault)]if rollback else[])+['-e',SCRIPT.read_text(),json.dumps(plan)]
        p=subprocess.run(command,cwd=root,env=env,capture_output=True,text=True,timeout=5)
        calls=[json.loads(line)for line in log.read_text().splitlines()]if log.exists()else[]
        assert all(c['private_variable_present']and not c['deadline_exposed']and not c['remaining_exposed']for c in calls)
        result={'case':name,'exit_code':p.returncode,'child_scripts':[c['script']for c in calls]}
        if 'round_deadline_exceeded'in p.stderr:result['terminal']='round_deadline_exceeded'
        elif 'runtime_deadline_input_invalid'in p.stderr:result['terminal']='runtime_deadline_input_invalid'
        results.append(result)
        return result
    now=lambda:int(time.time()*1000)
    r=run('uncontrolled');assert r['exit_code']==0 and r['child_scripts']==['setup','start']
    r=run('controlled_success',now()+3000,3000);assert r['exit_code']==0 and r['child_scripts']==['setup','start']
    r=run('expired_before_first_script',now()-1,0);assert r['child_scripts']==[]and r['terminal']=='round_deadline_exceeded'
    r=run('setup_finishes_after_deadline',now()+250,250,delay=450);assert r['child_scripts']==['setup']and r['terminal']=='round_deadline_exceeded'
    r=run('wall_clock_rollback',now()+2000,250,delay=450,rollback=True);assert r['child_scripts']==['setup']and r['terminal']=='round_deadline_exceeded'
    r=run('missing_remaining',now()+2000);assert r['child_scripts']==[]and r['terminal']=='runtime_deadline_input_invalid'
    r=run('invalid_deadline','not-a-clock',2000);assert r['child_scripts']==[]and r['terminal']=='runtime_deadline_input_invalid'
    r=run('unsafe_integer',2**60,2000);assert r['child_scripts']==[]and r['terminal']=='runtime_deadline_input_invalid'
print(json.dumps({'scope':'actual wrapper/child-process regression; not real Runtime acceptance',
    'pass':True,'cases':results,'paid_provider_calls':0},indent=2))
