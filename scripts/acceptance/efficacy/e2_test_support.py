"""Synthetic telemetry for offline tests only; no fixture code is executed."""
from copy import deepcopy
import json
from pathlib import Path
from types import SimpleNamespace

from e2_protocol import ARM_IDENTITY, PLAN, digest, registered_budget, safe_record

PLAN_DATA = json.loads(PLAN.read_text())


def snapshot(arm='C', case='D01', permutation=0, passed=True):
    _, model, prompt, schema = ARM_IDENTITY[arm]
    contract, parent, generated = 'sha256:K', 'sha256:parent', 'sha256:generated'
    receipt = {'fully_satisfied': passed, 'contract_ref': contract, 'derivation_ref': generated,
               'execution': {'attempt_id': 'attempt-generated', 'request_id': 'request'}}
    attempt = {'attempt_id': 'attempt-generated', 'runtime_id': 'runtime', 'environment_id': 'native',
               'derivation_ref': generated, 'status': 'pass' if passed else 'fail',
               'attestation': {'execution_started': True, 'attempt_record': 'finished'},
               'failure': None if passed else {'code': 'http_status_mismatch'},
               'formation_attempt': {'receipt': receipt}}
    parent_attempt = deepcopy(attempt)
    parent_attempt.update(attempt_id='attempt-parent', derivation_ref=parent, status='fail',
                          failure={'code': 'http_status_mismatch'})
    parent_receipt = parent_attempt['formation_attempt']['receipt']
    parent_receipt.update(derivation_ref=parent, fully_satisfied=False)
    parent_receipt['execution']['attempt_id'] = 'attempt-parent'
    route = {'attempt_id': 'attempt-generated', 'derivation_ref': generated,
             'effective_contract_ref': contract, 'runtime_id': 'runtime', 'environment_id': 'native',
             'verifier_receipts': [{'kind': 'contract_verification', 'receipt': deepcopy(receipt)}]}
    return {
        'requester_exit': 0, 'satisfy_id': 'request', 'generated_derivation_ref': generated,
        'generation_rows': [{'outcome': 'admitted', 'derivation_ref': generated, 'draft_json': '{}'}],
        'provider_request': {'state': {'schema': schema}}, 'provider_call_count': 1,
        'models': [model], 'prompt_versions': [prompt],
        'calls': [{'submission_accepted': True, 'answer': {}}],
        'usage': [{'input_tokens': 12, 'output_tokens': 3}], 'estimated_cost_usd': 0.000001,
        'provider_latency_ms': [2], 'elapsed_seconds': 1,
        'selected_entrypoint': 'k4', 'read_errors': [],
        'result': {'contract_ref': contract, 'satisfy_id': 'request',
                   'status': 'satisfied' if passed else 'unsatisfied',
                   'attempts': [parent_attempt, attempt], 'verified_routes': [route] if passed else [],
                   'search_state': {'frozen': {
                       'contract_ref': contract, 'candidates': [{'derivation_ref': parent}],
                       'policy': {'runtime_constraint': {'kind': 'exact', 'runtime_id': 'runtime'},
                                  'budget': registered_budget(PLAN_DATA),
                                  'generation': {'max_generations': 1, 'timeout_ms': 30000,
                                                 'entrypoints': PLAN_DATA['fixtures'][case]['permutations'][permutation]}}}}}}


def record(arm='C', case='D01', permutation=0, passed=True, outcome='admitted'):
    observed = snapshot(arm, case, permutation, passed)
    if outcome != 'admitted':
        observed['generation_rows'] = [{'outcome': outcome}]
        observed['generated_derivation_ref'] = None
        observed['result']['attempts'] = observed['result']['attempts'][:1]
        observed['result']['status'] = 'unsatisfied'
        observed['result']['verified_routes'] = []
    run = SimpleNamespace(root=Path('/nonexistent-e2-test-record'),
                          fixture_sha256=digest(PLAN_DATA['fixtures'][case]['files']))
    return safe_record(run, observed, case, permutation, arm, PLAN_DATA)


def terminal_snapshot(case='C01', entry='v9'):
    observed = snapshot('oracle', case, 0, False)
    observed['search_id'] = observed['result']['search_id'] = 'search'
    observed['generation_rows'][0]['search_id'] = 'search'
    observed['selected_entrypoint'] = entry
    observed['result']['unknown_attempts'] = []
    attempt = observed['result']['attempts'][-1]
    attempt['metadata_mismatch'] = []
    attempt['failure'] = {'code': 'candidate_not_observable', 'stage': 'verification', 'message': 'not used'}
    contract = observed['result']['contract_ref']
    attempt['attestation'].update(contract_ref=contract, derivation_ref=attempt['derivation_ref'])
    attempt['formation_attempt'] = {
        'attempt_id': attempt['attempt_id'], 'runtime_id': attempt['runtime_id'],
        'contract_ref': contract, 'derivation_ref': attempt['derivation_ref'], 'status': 'failed',
        'receipt': None, 'verification': None, 'failure': deepcopy(attempt['failure']),
        'realization': {'destroyed': True, 'executor': 'runtime-process',
                        'endpoints': {'app.http': 'guest 8000 -> host 8000'}},
        'outcomes': {'seal': {'state': 'failed', 'reason': 'candidate_not_observable'},
                     'runtime_verification': {'state': 'failed', 'reason': 'candidate_not_observable'},
                     'cleanup': {'state': 'succeeded'}},
    }
    return observed


def oracle_record(cell):
    if cell['expected_result'] != 'terminal_not_observable':
        r = record('oracle', cell['case'], 0, cell['expected_result'] == 'verified_pass')
        r['selected_entrypoint'] = r['evidence']['selected_entrypoint'] = cell['draft_entrypoint_id']
        return r
    observed = terminal_snapshot(cell['case'], cell['draft_entrypoint_id'])
    run = SimpleNamespace(root=Path('/nonexistent-e2-test-record'),
                          fixture_sha256=digest(PLAN_DATA['fixtures'][cell['case']]['files']))
    return safe_record(run, observed, cell['case'], 0, 'oracle', PLAN_DATA)
