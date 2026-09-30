#!/usr/bin/env python3
"""Aggregate actual immutable exploration output; never run/score applications."""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from datetime import datetime, timezone
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
        assert not state.get('verified_routes'), 'exploration unexpectedly created normal verified-route authorization'
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
        executed=[a for a in attempts if (a.get('attestation') or {}).get('execution_started')]
        # route_accepted in the requester summary describes the completed route,
        # not admission. A started contained execution proves admission even
        # when launch/readiness subsequently fails.
        admitted=[a for a in attempts if (a.get('attestation') or {}).get('execution_started')]
        verified=[a for a in attempts if (a.get('formation_attempt') or {}).get('verification')
                  or (a.get('formation_attempt') or {}).get('receipt')]
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
            'decision_provider_no_call_reason':('no_ambiguous_finite_choice_or_choice_point_not_reached' if dp['calls']==0 else None),
            'decision_provider_model':result.get('decision_provider_accounting',{}).get('model'),
            'decision_provider_responses':result.get('decision_provider_accounting',{}).get('responses',[]),
            'proposal_generated_D_count':len(generated),'generated_D_refs':sorted(generated),
            'known_D_refs':sorted(d['derivation_ref'] for d in known),
            'generated_D_admitted':sum(a['derivation_ref'] in generated for a in admitted),
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
            'baseline_permission_blocked_to_execution':app['baseline_terminal'] in ('network_denied','authority_denied','effect_denied') and bool(executed),
            'baseline_permission_blocked_to_PASS':app['baseline_terminal'] in ('network_denied','authority_denied','effect_denied') and success,
            'submission':submission,'final_requirements':result.get('final_requirements'),
            'final_requirement_basis':result.get('final_requirement_basis'),
            'permission_reduction':result.get('permission_reduction'),
            'provider_calls':result.get('candidate_producer_calls',[]),
            'proposal_outcomes':[{ 'round_seq':r.get('round_seq'),
                'raw_output_digest':r.get('raw_output_digest'),
                'outcomes':[{k:o[k] for k in ('status','code','derivation_ref') if k in o}
                            for o in r.get('outcomes',[])]}
                for r in result.get('proposal_evidence',[])],
            'attempt_evidence':[{k:a[k] for k in ('attempt_id','derivation_ref','status','failure','attestation') if k in a}
                for a in attempts],
            'rounds_consumed':result.get('rounds_consumed',summary.get('rounds_consumed',0)),
            'effective_max_rounds':result.get('effective_max_rounds'),
            'known_attempts':result.get('known_attempts',0),
            'generated_attempts':result.get('generated_attempts',0),
            'infrastructure_observations': ([{
                'code':'source_upload_quota_before_search_creation',
                'search_rounds_and_model_calls_before_recovery':0,
                'recovery':'same original Search ID, K, source, frozen ceiling and journals; fixed receiver index partition',
                'expired_open_round_during_requester_recompilation':'consumed; not reset or retried',
            }] if app['index']==plan.get('uncreated_search_recovery_index') else []),
            'functional_candidate':success and not app['baseline_typed_K_pass'],
            'functional_acceptance':'not_measured','deployed':False,'approval':'not_assessed'}
        row['normal_verified_routes']=len(state.get('verified_routes',[]))
        rows.append(row)
        for f in sorted(cell.rglob('*')):
            if f.is_file() and not any(x in ('scratch','requester','transport-recovery-work') for x in f.relative_to(cell).parts):
                raw.append({'path':str(f.relative_to(root)),'bytes':f.stat().st_size,'sha256':sha(f)})
    return rows,raw

def main():
    p=argparse.ArgumentParser();p.add_argument('--plan',required=True);p.add_argument('--run',required=True);p.add_argument('--out',required=True);p.add_argument('--baseline',required=True)
    a=p.parse_args();plan=read(a.plan);root=Path(a.run);out=Path(a.out);out.mkdir(parents=True,exist_ok=True)
    assert sha(a.baseline)==plan['baseline']['sha256']
    historical={app['index']:app for app in read(a.baseline)['applications']}
    rows,raw=aggregate(plan,root);assert len(rows)==100 and len({r['index'] for r in rows})==100
    for row in rows:
        old=historical[row['index']]
        for field in ('repo','ref','archive_sha256','license','license_sha256'):
            assert row[field]==old[field]
        assert row['baseline_terminal']==old['terminal_code']
        old_K=old.get('attempted_contract_refs',[])
        if old_K:assert row['contract_ref'] in old_K
        row['baseline_K_refs']=old_K
        row['baseline_D_refs']=old.get('attempted_derivation_refs',[])
        row['baseline_progress']=old['progress']
        row['reach_improved_layers']=[k for k,v in row['funnel'].items() if v and not old['progress'].get(k[0],False)]
        row['reach_reduced_layers']=[k for k,v in row['funnel'].items() if not v and old['progress'].get(k[0],False)]
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
        'actual_LLM_call_rate':sum(r['producer_calls']>0 or r['decision_calls']>0 for r in rows)/len(rows),
        'candidate_producer_calls':cp,'decision_provider_calls':dp,
        'valid_generated_D_apps':sum(r['proposal_generated_D_count']>0 for r in rows),
        'generated_D_admitted_apps':sum(r['generated_D_admitted']>0 for r in rows),
        'generated_D_executed_apps':sum(r['generated_D_executed']>0 for r in rows),
        'generated_D_PASS_apps':sum(r['generated_D_PASS']>0 for r in rows),
        'authority_recovered_to_execution_apps':sum(r['authority_recovered_to_execution'] for r in rows),
        'network_recovered_to_execution_apps':sum(r['network_recovered_to_execution'] for r in rows),
        'dependency_recovered_to_PASS_apps':sum(r['dependency_recovered_to_PASS'] for r in rows),
        'baseline_permission_blocked_to_execution_apps':sum(r['baseline_permission_blocked_to_execution'] for r in rows),
        'baseline_permission_blocked_to_PASS_apps':sum(r['baseline_permission_blocked_to_PASS'] for r in rows),
        'known_usage_estimated_cost_usd_micros':estimated,'conservative_charged_usd_micros':charged,
        'estimated_cost_per_additional_PASS_usd_micros':estimated/len(added) if added else None,
        'charged_cost_per_additional_PASS_usd_micros':charged/len(added) if added else None,
        'calls_per_additional_PASS':(cp+dp)/len(added) if added else None,
        'unresolved_model_reservations':0,'new_functional_acceptance':0}
    totals['normal_verified_route_authorizations']=sum(r['normal_verified_routes'] for r in rows)
    for field in ('input_tokens','output_tokens','unknown_usage_calls'):
        totals[field]=sum(r['candidate_producer_accounting'][field]+r['decision_provider_accounting'][field] for r in rows)
    totals['tokens_per_additional_PASS']=(totals['input_tokens']+totals['output_tokens'])/len(added) if added else None
    cp_provenance=[c['provenance'] for r in rows for c in r['provider_calls'] if c.get('provenance')]
    totals['candidate_producer_models']=dict(Counter(c['model'] for c in cp_provenance))
    totals['candidate_producer_latency_ms_recorded']=sum(c.get('latency_ms',0) for c in cp_provenance)
    totals['candidate_producer_calls_with_recorded_latency']=sum('latency_ms' in c for c in cp_provenance)
    totals['decision_provider_models_configured']=sorted({r['decision_provider_model'] for r in rows if r['decision_provider_model']})
    totals['candidate_producer_input_tokens']=sum(r['candidate_producer_accounting']['input_tokens'] for r in rows)
    totals['candidate_producer_output_tokens']=sum(r['candidate_producer_accounting']['output_tokens'] for r in rows)
    totals['decision_provider_input_tokens']=sum(r['decision_provider_accounting']['input_tokens'] for r in rows)
    totals['decision_provider_output_tokens']=sum(r['decision_provider_accounting']['output_tokens'] for r in rows)
    totals['requester_elapsed_seconds_sum']=round(sum(r.get('elapsed_seconds',0) for r in rows),3)
    start=(root/'cells'/'001'/'preflight-config.json').stat().st_mtime
    end=max((root/'cells'/f"{r['index']:03}"/'summary.json').stat().st_mtime for r in rows)
    totals['wall_elapsed_seconds_including_infrastructure_recovery']=round(end-start,3)
    totals['started_at']=datetime.fromtimestamp(start,timezone.utc).isoformat()
    totals['completed_at']=datetime.fromtimestamp(end,timezone.utc).isoformat()
    totals['reach_improved_apps']=sum(bool(r['reach_improved_layers']) for r in rows)
    totals['reach_reduced_apps']=sum(bool(r['reach_reduced_layers']) for r in rows)
    totals['rounds_consumed']=sum(r['rounds_consumed'] for r in rows)
    totals['known_attempts']=sum(r['known_attempts'] for r in rows)
    totals['generated_attempts']=sum(r['generated_attempts'] for r in rows)
    ledger={'schema':'ato.formation-exploration-100-ledger/1','plan_sha256':sha(a.plan),
        'ato_pin':plan['ato_pin'],'ato_api_pin':plan['ato_api_pin'],'totals':totals,'funnel':funnel,
        'baseline':plan['baseline'],
        'primary_distribution':dict(Counter(r['primary'] for r in rows)),
        'stop_reason_distribution':dict(Counter(r['adaptive_terminal'].get('reason',r['adaptive_terminal'].get('code','unknown')) for r in rows)),
        'proposal_disposition_distribution':dict(Counter(o.get('status','unknown') for r in rows for p in r['proposal_outcomes'] for o in p['outcomes'])),
        'funnel_definitions':{'D_admitted':'conservatively proven by execution_started attestation, including later startup failures',
            'F_verifier_ran':'actual verification verdicts or fresh receipt; readiness failure alone is not Verifier observation'},
        'distributions':distributions,'applications':rows,'raw_evidence_manifest':raw,
        'functional_evidence':'previous 2048/reveal append separate; none newly measured',
        'conditions_changed_from_baseline':plan['entry_conditions'],'merged':False,'deployed':False}
    put(out/'formation-exploration-100.json',ledger)
    put(out/'formation-known-vs-exploration-100.json',{'schema':'ato.formation-known-vs-exploration/1',
        'totals':totals,'applications':[{'index':r['index'],'name':r['name'],
        'baseline_terminal':r['baseline_terminal'],'baseline_PASS':r['baseline_typed_K_pass'],
        'baseline_K_refs':r['baseline_K_refs'],'baseline_D_refs':r['baseline_D_refs'],
        'baseline_progress':r['baseline_progress'],'current_K_ref':r['contract_ref'],
        'current_known_D_refs':r['known_D_refs'],
        'current_generated_D_refs':r['generated_D_refs'],'current_progress':r['funnel'],
        'reach_improved_layers':r['reach_improved_layers'],'reach_reduced_layers':r['reach_reduced_layers'],
        'adaptive_terminal':r['adaptive_terminal'],'adaptive_PASS':r['typed_K_pass'],
        'change':'regression' if r['index'] in regressed else 'reach_improvement' if r['index'] in added else 'unchanged_outcome',
        'producer_calls':r['producer_calls'],'decision_calls':r['decision_calls']} for r in rows]})
    put(out/'formation-exploration-100-raw-manifest.json',raw)
    print(json.dumps(totals,indent=2))
if __name__=='__main__':main()
