#!/usr/bin/env python3
"""Preregistered E1 only: 60 cells, <=20 Jev calls, never retry a cell.

--plan writes file digests without invoking Runtime/provider. --run requires a
committed registration and model key through stdin; historical G9 is never used.
Run only on isolated stage5b-api, with already initialized local migration0303.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from efficacy.fixtures import CASES, materialize

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
PLAN = ROOT / 'docs/ops/formation-efficacy-e1-amendment.json'

def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()

def plan_fixtures(output):
    notes = ROOT / 'apps/formation-worker/fixtures/runtime-network/notes'
    output.mkdir(parents=True, exist_ok=False)
    fixtures = {}
    for case in CASES:
        entries, files = materialize(notes, output / case, case, 0)
        fixtures[case] = {'files': files, 'correct_candidate_indices': [0, 1] if case == 'E02' else [] if case == 'E10' else [0],
                          'permutations': [entries, {k: ('candidate_1.py' if v == 'candidate_0.py' else 'candidate_0.py') for k, v in entries.items()}]}
    print(json.dumps(fixtures, indent=2))

def safe_record(run, snapshot, case, permutation, arm, error=None):
    result = snapshot.get('result') or {}
    attempts = result.get('attempts') or []
    rows = snapshot.get('generation_rows') or []
    request = snapshot.get('provider_request') or {}
    receipts = [r for r in snapshot.get('receipts', []) if r]
    frozen = (result.get('search_state') or {}).get('frozen', {})
    exact = (frozen.get('policy') or {}).get('runtime_constraint', {}).get('runtime_id')
    succeeded = (not error and snapshot.get('requester_exit') == 0 and result.get('status') == 'satisfied'
                 and bool(receipts) and receipts[-1].get('fully_satisfied') is True
                 and all(r.get('contract_ref') == result.get('contract_ref') for r in receipts))
    outcome = rows[0]['outcome'] if rows else None
    context = request.get('state')
    violations = []
    if any(a.get('runtime_id') != exact for a in attempts): violations.append('exact_escape')
    if len(rows) > 1 or snapshot.get('provider_call_count', 0) > 1: violations.append('repeat_generation')
    if any(r.get('contract_ref') != result.get('contract_ref') for r in receipts): violations.append('K_changed')
    if any(a.get('status') == 'unknown' for a in attempts) and rows: violations.append('unknown_generation')
    if context and 'EFFICACY_PRIVATE_CANARY' in json.dumps(context): violations.append('canary_leak')
    return {'case':case,'permutation':permutation,'arm':arm,'same_k_success':bool(succeeded),
            'attempts':len(attempts),'attempts_to_pass':len(attempts) if succeeded else None,
            'generation_outcome':outcome,'admitted':outcome == 'admitted','declined':outcome == 'declined',
            'invalid_rejected':outcome in ('invalid','out_of_set','duplicate'),
            'draft_proposed':bool(rows and rows[0].get('draft_json')),
            'selected_entrypoint':snapshot.get('selected_entrypoint'),
            'generated_derivation_ref':snapshot.get('generated_derivation_ref'),
            'contract_ref':result.get('contract_ref'),'fixture_hash':run.fixture_sha256,
            'context':context,'context_hash':digest(context) if context else None,
            'provider_calls':snapshot.get('provider_call_count',0),
            'model_calls':snapshot.get('provider_call_count',0) if arm == 'C' else 0,
            'models':snapshot.get('models'),'prompt_versions':snapshot.get('prompt_versions'),
            'usage':snapshot.get('usage'),'cost_usd':snapshot.get('estimated_cost_usd') if arm == 'C' else 0,
            'provider_latency_ms':snapshot.get('provider_latency_ms'),'elapsed_seconds':snapshot.get('elapsed_seconds'),
            'requester_exit':snapshot.get('requester_exit'),'error':error,'violations':violations,
            'receipt': [{'fully_satisfied':r['fully_satisfied'],'contract_ref':r['contract_ref'],
                         'derivation_ref':r['derivation_ref']} for r in receipts],
            'budget': (result.get('search_state') or {}).get('frozen',{}).get('policy',{}).get('budget'),
            'report_sha256':hashlib.sha256((run.root/'report.json').read_bytes()).hexdigest()}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('--plan', type=Path)
    group.add_argument('--run', action='store_true')
    args = parser.parse_args()
    if args.plan:
        plan_fixtures(args.plan); return
    plan = json.loads(PLAN.read_text())
    key = sys.stdin.readline().strip()
    if not key: raise RuntimeError('dedicated generation key missing')
    spec = importlib.util.spec_from_file_location('context_acceptance', HERE/'formation-generation-context.py')
    c = importlib.util.module_from_spec(spec); sys.modules[spec.name] = c; spec.loader.exec_module(c)
    c.load_helpers()
    h, d, v1 = c.h, c.d, c.v1
    if not (h.API_DIR/'initialized-0303').exists(): raise RuntimeError('local0303 not initialized')
    for name,path in [('requester',h.REQ),('runtime',h.ATO),('wasm',h.API_DIR/'worker-final-bundle/receipt_authority.wasm')]:
        if hashlib.sha256(path.read_bytes()).hexdigest() != plan['artifacts'][name]: raise RuntimeError('artifact drift: '+name)
    original = h.S4 / 'efficacy-e1'
    out = h.S4 / 'efficacy-e1-repaired'
    out.mkdir(exist_ok=False)  # whole experiment once, no resume/retry
    h.LEDGER.close(); h.OUT = v1.OUT = c.OUT = out
    h.LEDGER = (out/'ledger.jsonl').open('x')
    (out/'registration.json').write_bytes(PLAN.read_bytes())
    original_results = json.loads((original/'results.json').read_text())
    # Retain every original A/C observation, including model declines. Only the
    # broken local comparator is replaced under the prospectively registered amendment.
    results = [r for r in original_results if r['arm'] != 'B']
    reused = {f"{r['case']}p{r['permutation']}{r['arm']}" for r in results}
    reserved = {p.stem for p in original.glob('*.reserved') if not p.stem.endswith('B')}
    if reused != reserved or sorted(reused) != sorted(plan['reuse_cells']):
        raise RuntimeError('ambiguous old A/C reservation: never re-call')
    if hashlib.sha256((original/'results.json').read_bytes()).hexdigest() != plan['original_results_sha256']:
        raise RuntimeError('original observations changed')
    h.start_coordinator('efficacy')
    try:
        for case in CASES:
            for permutation in (0,1):
                for arm in ('ABC' if permutation == 0 else 'CBA'):
                    cell = f'{case}p{permutation}{arm}'
                    if cell in reused:
                        continue
                    # Persistent reservation before startup; never removed.
                    (out/f'{cell}.reserved').write_text('reserved\n')
                    root = out/cell; root.mkdir()
                    d.OWNER = f'efficacy_{cell}_{v1.RUN_ID}'; d.TOKEN = out/f'token-{cell}'; d.ensure_owner()
                    h.OWNER,h.TOKEN = d.OWNER,d.TOKEN
                    entries, hashes = materialize(h.FIX/'notes', root/'source', case, permutation)
                    assert hashes == plan['fixtures'][case]['files'], 'fixture drift'
                    assert entries == plan['fixtures'][case]['permutations'][permutation], 'mapping drift'
                    parent = root/'base.toml'
                    parent.write_text((root/'source/capsule.toml').read_text().replace('/app/app.py','/app/bad.py'))
                    routes = [parent]
                    known = 2 if case == 'E09' else 1
                    if known == 2:
                        second = root/'base-b.toml'; second.write_text(parent.read_text().replace('/app/bad.py','/app/bad_b.py')); routes.append(second)
                    run = c.Run(cell,root,f'efficacy{cell}{v1.RUN_ID}',entries=entries,owner=d.OWNER,token=d.TOKEN)
                    run.fixture_sha256 = digest(hashes)
                    snapshot = {}; error = None
                    try:
                        run.runtime = h.runtime(cell,'runtime',known+1)
                        rid = h.sql('SELECT id FROM runner_devices WHERE user_id=?',d.OWNER)[0]['id']
                        h.wait_for(lambda:h.sql('SELECT environment_id FROM runtime_environments WHERE runtime_id=?',rid),'runtime',timeout=120)
                        env = {k:v for k,v in h.ENV.items() if not k.startswith('ATO_ACCEPTANCE_') and k not in ('ATO_GENERATION_JEV_API_KEY','ATO_DECISION_JEV_API_KEY')}
                        env.update(ATO_ACCEPTANCE_SETTLE_SECS='180',ATO_ACCEPTANCE_EXACT_RUNTIME=rid)
                        if arm != 'A':
                            env.update(ATO_ACCEPTANCE_GENERATION_ENTRYPOINTS=json.dumps(entries),
                                       ATO_ACCEPTANCE_GENERATION_CONTEXT='v2',
                                       ATO_ACCEPTANCE_GENERATION_PROVIDER='jev_v2' if arm == 'C' else 'deterministic_v1',
                                       ATO_ACCEPTANCE_GENERATION_LOG=str(root/'generation-calls.jsonl'),
                                       ATO_ACCEPTANCE_GENERATION_INPUT=str(root/'provider-request.json'),
                                       ATO_GENERATION_JEV_MODEL=plan['model'])
                        if arm == 'C': env['ATO_GENERATION_JEV_API_KEY'] = key
                        if case == 'E09':
                            env.update(ATO_ACCEPTANCE_DECISION_POLICY='4,30000',
                                       ATO_ACCEPTANCE_DECISION_PROVIDER='seq:0=0,1=2,2=0,3=0',
                                       ATO_ACCEPTANCE_DECISION_LOG=str(root/'decision-calls.jsonl'))
                        with (root/'status.json').open('w') as stdout,(root/'request.log').open('w') as stderr:
                            run.requester = subprocess.Popen([str(h.REQ),h.API,str(d.TOKEN),str(root/'source'),str(root/'work'),run.sid,str(known+1),str(root/'created.json'),*map(str,routes)],env=env,stdout=stdout,stderr=stderr,start_new_session=True)
                        h.wait_for(lambda:(root/'created.json').exists() and (root/'created.json').read_text(),'created',timeout=120)
                        run.satisfy = c.read_json(root/'created.json')['satisfy_id']
                        snapshot = c.settle(run)
                    except Exception as exc:
                        error = type(exc).__name__ + ': ' + str(exc)
                        snapshot = c.observe(run,'error',error=error)
                    finally:
                        c.cleanup(run)
                        d.TOKEN.unlink(missing_ok=True)
                    record = safe_record(run,snapshot,case,permutation,arm,error)
                    # Compare production context, K and budget without changing any outcome.
                    peers = [x for x in results if x['case']==case and x['permutation']==permutation]
                    for peer in peers:
                        if peer['contract_ref'] != record['contract_ref']: record['violations'].append('cross_arm_K_drift')
                        if peer['budget'] != record['budget']: record['violations'].append('cross_arm_budget_drift')
                        if peer['context'] and record['context'] and peer['context'] != record['context']: record['violations'].append('cross_arm_context_drift')
                    results.append(record)
                    (out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
                    print(json.dumps({k:record[k] for k in ['case','permutation','arm','same_k_success','generation_outcome','attempts','error','violations']}),flush=True)
                    if record['violations']: raise RuntimeError('safety/input drift; experiment stopped')
    finally:
        h.stop_coordinator()
        (out/'results.json').write_text(json.dumps(results,indent=2)+'\n')

if __name__ == '__main__': main()
