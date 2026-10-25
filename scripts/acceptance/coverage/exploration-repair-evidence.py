#!/usr/bin/env python3
"""Offline repair append. Raw manifests and requester/Coordinator facts are authority."""
import argparse
import base64
import hashlib
import json
from pathlib import Path


def read(p):
    return json.loads(Path(p).read_text())


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def case(root, label, index):
    cell = root / label / 'cells' / f'{index:03}'
    result = read(cell / 'requester.stdout.log')
    status = read(cell / 'status.json')
    summary = read(cell / 'summary.json')
    submission = result['submission']
    if submission:
        receipt = submission['receipt']
        assert receipt['fully_satisfied']
        assert receipt['contract_ref'] == result['contract_ref'] == submission['contract_ref']
        assert receipt['derivation_ref'] == submission['derivation_ref']
        assert receipt['execution']['attempt_id'] == submission['attempt_id']
        assert submission['status'] == 'k_reached_awaiting_assessment'
    assert result['approval'] == 'not_assessed' and not result['deployed']
    assert not status['verified_routes']
    cells = result['candidate_producer_accounting']['cells']
    assert all(c.get('response') for c in cells.values()), 'unresolved call'
    projections = [c['request']['transmitted_context'] for c in cells.values()]
    proposals = []
    for evidence, projection in zip(result['proposal_evidence'], projections, strict=True):
        raw = base64.b64decode(evidence['raw_output_base64'])
        assert 'sha256:' + hashlib.sha256(raw).hexdigest() == evidence['raw_output_digest']
        try:
            batch = json.loads(raw)
            raw_validation = 'well_formed'
        except json.JSONDecodeError as error:
            batch = {'proposals': []}
            raw_validation = {'code': 'invalid_json', 'position': error.pos}
        plans = []
        for proposal in batch['proposals']:
            for operation in proposal.get('operations', []):
                plan = operation.get('plan', {})
                plans.append({
                    'execution': {k: plan.get(k) for k in ('runtime', 'entrypoint', 'argv', 'cwd',
                        'module', 'guest_port', 'static_output', 'dependencies', 'build_scripts',
                        'state', 'requirements')},
                    'environment_keys_only': sorted(plan.get('environment', {})),
                    'diagnostic_fields_present': {k: k in plan for k in ('basis', 'unknowns')},
                })
        proposals.append({'round_seq': evidence['round_seq'], 'raw_sha256': evidence['raw_output_digest'],
            'raw_json_validation': raw_validation,
            'kinds': [p['kind'] for p in batch['proposals']],
            'decline_reasons': [p.get('reason') for p in batch['proposals'] if p['kind'] == 'unsupported'],
            'outcomes': [{k: o[k] for k in ('status', 'code', 'derivation_ref') if k in o}
                         for o in evidence['outcomes']],
            'plans': plans, 'actually_transmitted_context': projection})
    calls = result['candidate_producer_calls']
    return {'label': label, 'index': index, 'name': summary['name'], 'typed_K_pass': bool(submission),
        'contract_ref': result['contract_ref'], 'fresh_submission': submission,
        'final_requirements': result['final_requirements'],
        'final_requirement_basis': result['final_requirement_basis'],
        'permission_reduction': result['permission_reduction'],
        'rounds_consumed': result['rounds_consumed'], 'known_attempts': result['known_attempts'],
        'generated_attempts': result['generated_attempts'], 'attempts': result['attempts'],
        'stop': result['stop'], 'elapsed_seconds': summary['elapsed_seconds'],
        'rounds': proposals, 'round_timing':result['rounds'], 'attempt_timing':[{'claimed_at':a.get('claimed_at'),'finished_at':a.get('finished_at'),'derivation_ref':a['derivation_ref'],'status':a['status']} for a in status['attempts']], 'calls': calls, 'CP_calls': len(calls), 'DP_calls': summary['decision_calls'],
        'input_tokens': sum(c['provenance']['usage']['input_tokens'] for c in calls),
        'output_tokens': sum(c['provenance']['usage']['output_tokens'] for c in calls),
        'estimated_cost_usd_micros': sum(c['provenance']['estimated_cost_usd_micros'] for c in calls),
        'conservative_charge_usd_micros': len(calls) * 9831,
        'functional_acceptance': 'not_measured', 'persistence': 'not_measured',
        'normal_verified_routes': 0, 'unresolved_model_reservations': 0,
        'result_sha256': sha(cell / 'requester.stdout.log'), 'status_sha256': sha(cell / 'status.json')}


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--root', required=True)
    p.add_argument('--manifest', required=True)
    p.add_argument('--output', required=True)
    a = p.parse_args()
    root = Path(a.root)
    for f in read(a.manifest)['files']:
        path = root / f['path']
        assert path.resolve().is_relative_to(root.resolve())
        assert path.stat().st_size == f['bytes'] and sha(path) == f['sha256']
    cases = [case(root, label, n) for label, n in
             [('zero', 57), ('zero', 69), ('replay', 57), ('final', 57), ('metadata', 57)]]
    wbo = root / 'interrupted-wbo/cells/002'
    observed = read(wbo / 'status.observed-after-harness-stop.json')
    journal = [json.loads(line) for line in (wbo / 'producer.jsonl').read_text().splitlines()]
    responses = [e['response'] for e in journal if 'response' in e]
    assert len(responses) == 1 and observed['attempts'][0]['status'] == 'unknown'
    prior = responses[0]
    interrupted = {'label': 'interrupted-wbo', 'index': 2, 'typed_K_pass': False,
        'classification': 'untyped requester wait expiry, harness parse error; Coordinator UNKNOWN',
        'application_terminal': 'not_established', 'automatic_retry': False, 'CP_calls': 1, 'DP_calls': 0,
        'input_tokens': prior['input_tokens'], 'output_tokens': prior['output_tokens'],
        'estimated_cost_usd_micros': (prior['input_tokens']*300000+prior['output_tokens']*1200000+999999)//1000000,
        'conservative_charge_usd_micros': 9831, 'unresolved_model_reservations': 0,
        'runtime_UNKNOWN': 1, 'status_sha256': sha(wbo / 'status.observed-after-harness-stop.json')}
    total = {k: sum(x[k] for x in [*cases, interrupted]) for k in
             ('CP_calls', 'DP_calls', 'input_tokens', 'output_tokens', 'estimated_cost_usd_micros', 'conservative_charge_usd_micros')}
    total.update(unique_additional_typed_K_apps=len({x['index'] for x in cases if x['typed_K_pass']}),
        pass_cases=sum(x['typed_K_pass'] for x in cases),
        remaining_usd_micros=722202-total['conservative_charge_usd_micros'],
        unresolved_model_reservations=0, unresolved_runtime_UNKNOWN=1,
        per_call_latency='not_recorded; case elapsed is not provider latency')
    output = {'schema': 'ato.formation-exploration-repair-ledger/1',
        'scope': 'bounded upstream repair pilots; not 100-app remeasurement',
        'original_100': {'baseline_typed_K': 7, 'exploration_typed_K': 7, 'additional': 0, 'unchanged': True},
        'pins': {'zero_and_replay': 'dfced9182159c143dd1ca994f42bf160eddc9f40',
            'final_failed': '2ad8c108b58a170a8673663d03ad8c7eaa6988fa',
            'metadata': '44772cb2f1347a353a1d2c0593af4481c5da1e61',
            'API_JS': 'b20bba7ab9daf282f8d5b1e22affe549ad3492a3'},
        'cases': cases, 'interrupted': interrupted, 'totals': total,
        'permission_recovery': {'original_100_network': [0,0,0,0,0],
            'original_100_authority': [2,0,0,0,0],
            'small_pilot_network_failure': 'WBO denied in logs, but never returned typed Runtime evidence; no recovery claim'},
        'raw_manifest_sha256': sha(a.manifest), 'implemented': True,
        'verified': 'real Coordinator/Runtime/live provider subset; per-case exact pins',
        'merged': False, 'deployed': False, 'remote_migration': False}
    Path(a.output).write_text(json.dumps(output, indent=2) + '\n')
    print(json.dumps(total))


if __name__ == '__main__':
    main()
