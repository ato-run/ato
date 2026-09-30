#!/usr/bin/env python3
"""Aggregate actual immutable exploration output; never run/score applications."""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path


def read(path): return json.loads(Path(path).read_text())
def sha(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def put(path, value): Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False)+'\n')

def codes(result, status):
    out=[]
    for a in result.get('attempts',[]):
        if a.get('failure_code'): out.append(a['failure_code'])
    for r in result.get('rounds',[]): out.extend(r.get('diagnostics',[]))
    for r in result.get('proposal_evidence',[]):
        for o in r.get('outcomes',[]):
            if o.get('code'): out.append(o['code'])
    for r in status.get('proposal_history',[]) + ([status['proposal_round']] if status.get('proposal_round') else []):
        out.extend(r.get('diagnostics',[]))
    return sorted(set(out))

def accounting(result, field):
    view=result.get(field,{})
    cells=view.get('cells',view.get('journal',{}).get('cells',{}))
    total={'calls':len(cells),'input_tokens':0,'output_tokens':0,'usage_known_calls':0,'unknown_usage_calls':0,'unresolved':0}
    for cell in cells.values():
        response=cell.get('response')
        if response:
            total['input_tokens']+=response.get('input_tokens',0)
            total['output_tokens']+=response.get('output_tokens',0)
            total['usage_known_calls']+=1
        elif cell.get('charged_unknown'):
            total['unknown_usage_calls']+=1
        else: total['unresolved']+=1
    return total

def primary(result, diagnostics, success):
    if success:return 'success'
    stop=(result.get('stop') or {}).get('reason')
    if stop in ('exploration_authority_exceeded','no_progress','budget_exhausted'):return stop
    if result.get('search_status')=='unknown':return 'unknown'
    text=' '.join(diagnostics)
    if 'unsupported_toolchain' in text:return 'unsupported_toolchain'
    if any(x in text for x in ('network','dependenc','hash_required','requirements_file','npm_','pip_')):return 'dependency/network'
    if 'authority' in text:return 'authority'
    if any(x in text for x in ('runtime','toolchain','entrypoint','source_oci','recipe')):return 'runtime/adapter'
    if 'verifier' in text:return 'verifier'
    if any(c.get('status')=='error' for c in result.get('candidate_producer_calls',[])):return 'provider_error'
    return stop or 'unknown'

def aggregate(plan, root):
    rows=[]; raw=[]
    for app in plan['applications']:
        cell=root/'cells'/f"{app['index']:03}"
        summary=read(cell/'summary.json')
        result=read(root/summary['result'])
        assert sha(root/summary['result'])==summary['result_sha256']
        source_terminal=result.get('preflight_terminal')
        state=read(cell/'status.json') if not source_terminal else {}
        success=summary['typed_K_pass']
        submission=result.get('submission')
        if success:
            receipt=submission['receipt']
            assert receipt['fully_satisfied'] and receipt['contract_ref']==app['contract_ref']==submission['contract_ref']
            assert receipt['derivation_ref']==submission['derivation_ref']
            assert receipt['execution']['attempt_id']==submission['attempt_id']
            assert result['approval']=='not_assessed' and result['deployed'] is False
        cp=accounting(result,'candidate_producer_accounting');dp=accounting(result,'decision_provider_accounting')
        assert cp['unresolved']==dp['unresolved']==0
        assert cp['calls']==summary['producer_calls'] and dp['calls']==summary['decision_calls']
        known=state.get('search_state',{}).get('frozen',{}).get('candidates',[])
        generated={d['derivation_ref'] for r in result.get('rounds',[]) for d in r.get('candidates',[])}
        attempts=state.get('attempts',[])
        admitted=[a for a in result.get('attempts',[]) if a.get('route_accepted')]
        executed=[a for a in attempts if (a.get('attestation') or {}).get('execution_started')]
        verified=[a for a in attempts if (a.get('formation_attempt') or {}).get('receipt')]
        diagnostic=codes(result,state) + ([source_terminal['code']] if source_terminal else [])
        failures=[a for a in attempts if a.get('failure')]
        authority_recovered=False;network_recovered=False; dependency_recovered=False
        transfer=0; denials=[]
        for i,a in enumerate(attempts):
            evidence=a.get('exploration_evidence') or []
            reports=[r for e in evidence if e.get('kind')=='exploration_network_evidence' for r in e.get('reports',[])]
            transfer+=max([r['report'].get('transferred_bytes',0) for r in reports] or [0])
            for report in reports:
                denials.extend({'phase':report['phase'],**d} for d in report['report'].get('refused',[]))
            failure=a.get('failure') or {}; code=failure.get('code',''); message=failure.get('message','')
            later=[b for b in attempts[i+1:] if (b.get('attestation') or {}).get('execution_started')]
            if code=='authority_denied' and later:authority_recovered=True
            if ('network' in code or any(r['report'].get('refused') for r in reports)) and later:network_recovered=True
            if any(s in message for s in ('MODULE_NOT_FOUND','ModuleNotFoundError','No module named')) and any(b.get('status')=='pass' for b in later):dependency_recovered=True
        no_call=('source_verification_or_transport_failure' if source_terminal else
            'fresh_known_D_PASS' if success and cp['calls']==0 else
            'budget_or_UNKNOWN_blocked' if result.get('search_status')=='unknown' or (result.get('stop') or {}).get('reason')=='budget_exhausted' else
            'provider_not_reached') if cp['calls']==0 else None
        charged=cp['calls']*plan['model_budget']['per_CP_reservation']+dp['calls']*plan['model_budget']['per_DP_reservation']
        estimate=0
        for field,inp,out in (('candidate_producer_accounting',300000,1200000),('decision_provider_accounting',42000,0)):
            view=result.get(field,{})
            for c in view.get('cells',view.get('journal',{}).get('cells',{})).values():
                if c.get('response'):
                    response=c['response']
                    estimate+=(response.get('input_tokens',0)*inp+999999)//1000000
                    estimate+=(response.get('output_tokens',0)*out+999999)//1000000
        row={**app,**summary,'adaptive_terminal':source_terminal or result.get('stop') or {'reason':result.get('search_status')},
            'primary': 'source/preflight' if source_terminal else primary(result,diagnostic,success),
            'secondary':diagnostic,'candidate_producer_no_call_reason':no_call,
            'proposal_generated_D_count':len(generated),'generated_D_refs':sorted(generated),
            'generated_D_executed':sum(a['derivation_ref'] in generated for a in executed),
            'generated_D_PASS':sum(a.get('status')=='pass' and a['derivation_ref'] in generated for a in attempts),
            'funnel':{'A_source_recognized':not bool(source_terminal),'B_K_formed':not bool(source_terminal),
                'C_D_available':bool(known or generated),'D_admitted':bool(admitted),
                'E_executed':bool(executed),'F_verifier_ran':bool(verified),
                'G_typed_K_PASS':success,'H_retained':any(a.get('retained_ref') for a in result.get('attempts',[]) if a.get('status')=='pass')},
            'candidate_producer_accounting':cp,'decision_provider_accounting':dp,
            'known_usage_estimated_cost_usd_micros':estimate,'conservative_charged_usd_micros':charged,
            'network_transfer_bytes_shared_counter':transfer,'network_denials':denials,
            'authority_recovered_to_execution':authority_recovered,'network_recovered_to_execution':network_recovered,
            'dependency_recovered_to_PASS':dependency_recovered,
            'submission':submission,'final_requirements':result.get('final_requirements'),
            'final_requirement_basis':result.get('final_requirement_basis'),
            'permission_reduction':result.get('permission_reduction'),
            'provider_calls':result.get('candidate_producer_calls',[]),
            'functional_candidate':success and not app['baseline_typed_K_pass'],
            'functional_acceptance':'not_measured','deployed':False,'approval':'not_assessed'}
        rows.append(row)
        for f in sorted(cell.rglob('*')):
            if f.is_file() and not any(x in ('scratch','requester') for x in f.relative_to(cell).parts):
                raw.append({'path':str(f.relative_to(root)),'bytes':f.stat().st_size,'sha256':sha(f)})
    return rows,raw

def main():
    p=argparse.ArgumentParser();p.add_argument('--plan',required=True);p.add_argument('--run',required=True);p.add_argument('--out',required=True)
    a=p.parse_args();plan=read(a.plan);root=Path(a.run);out=Path(a.out);out.mkdir(parents=True,exist_ok=True)
    rows,raw=aggregate(plan,root);assert len(rows)==100 and len({r['index'] for r in rows})==100
    funnel={k:sum(r['funnel'][k] for r in rows) for k in rows[0]['funnel']}
    baseline=sum(r['baseline_typed_K_pass'] for r in rows);current=sum(r['typed_K_pass'] for r in rows);gain=current-baseline
    added=[r['index'] for r in rows if r['typed_K_pass'] and not r['baseline_typed_K_pass']]
    regressed=[r['index'] for r in rows if not r['typed_K_pass'] and r['baseline_typed_K_pass']]
    cp=sum(r['producer_calls'] for r in rows);dp=sum(r['decision_calls'] for r in rows)
    estimated=sum(r['known_usage_estimated_cost_usd_micros'] for r in rows);charged=sum(r['conservative_charged_usd_micros'] for r in rows)
    distributions={}
    for field in ('primary_language','broad_source_shape'):
        groups=defaultdict(Counter)
        for r in rows:groups[r[field]][r['primary']]+=1
        distributions[field]={k:dict(v) for k,v in sorted(groups.items())}
    totals={'actual_terminals':100,'baseline_typed_K_PASS':baseline,'current_typed_K_PASS':current,
        'absolute_gain':gain,'new_PASS_apps':added,'regressed_apps':regressed,
        'candidate_producer_invoked_apps':sum(r['producer_calls']>0 for r in rows),
        'candidate_producer_calls':cp,'decision_provider_calls':dp,
        'valid_generated_D_apps':sum(r['proposal_generated_D_count']>0 for r in rows),
        'generated_D_executed_apps':sum(r['generated_D_executed']>0 for r in rows),
        'generated_D_PASS_apps':sum(r['generated_D_PASS']>0 for r in rows),
        'authority_recovered_to_execution_apps':sum(r['authority_recovered_to_execution'] for r in rows),
        'network_recovered_to_execution_apps':sum(r['network_recovered_to_execution'] for r in rows),
        'dependency_recovered_to_PASS_apps':sum(r['dependency_recovered_to_PASS'] for r in rows),
        'known_usage_estimated_cost_usd_micros':estimated,'conservative_charged_usd_micros':charged,
        'estimated_cost_per_additional_PASS_usd_micros':estimated/len(added) if added else None,
        'charged_cost_per_additional_PASS_usd_micros':charged/len(added) if added else None,
        'unresolved_model_reservations':0,'new_functional_acceptance':0}
    for field in ('input_tokens','output_tokens','unknown_usage_calls'):
        totals[field]=sum(r['candidate_producer_accounting'][field]+r['decision_provider_accounting'][field] for r in rows)
    ledger={'schema':'ato.formation-exploration-100-ledger/1','plan_sha256':sha(a.plan),
        'ato_pin':plan['ato_pin'],'ato_api_pin':plan['ato_api_pin'],'totals':totals,'funnel':funnel,
        'primary_distribution':dict(Counter(r['primary'] for r in rows)),
        'distributions':distributions,'applications':rows,'raw_evidence_manifest':raw,
        'functional_evidence':'previous 2048/reveal append separate; none newly measured',
        'conditions_changed_from_baseline':plan['entry_conditions'],'merged':False,'deployed':False}
    put(out/'formation-exploration-100.json',ledger)
    put(out/'formation-known-vs-exploration-100.json',{'schema':'ato.formation-known-vs-exploration/1',
        'totals':totals,'applications':[{'index':r['index'],'name':r['name'],
        'baseline_terminal':r['baseline_terminal'],'baseline_PASS':r['baseline_typed_K_pass'],
        'adaptive_terminal':r['adaptive_terminal'],'adaptive_PASS':r['typed_K_pass'],
        'change':'regression' if r['index'] in regressed else 'reach_improvement' if r['index'] in added else 'unchanged_outcome',
        'producer_calls':r['producer_calls'],'decision_calls':r['decision_calls']} for r in rows]})
    put(out/'formation-exploration-100-raw-manifest.json',raw)
    print(json.dumps(totals,indent=2))
if __name__=='__main__':main()
