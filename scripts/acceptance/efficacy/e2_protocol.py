"""Fail-closed E2 evidence and preregistration checks, shared by both runs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[3]
PLAN = ROOT / 'docs/ops/formation-efficacy-e2-plan.json'
CANARY = 'EFFICACY_E2_PRIVATE_CANARY'
ARM_IDENTITY = {
    'A': (0, 'deterministic-selector-v2', 'efficacy-selector/2', 'ato.formation-generation-context/2'),
    'B': (1, 'jev-1.13.0', 'ato.formation-generation-prompt/2', 'ato.formation-generation-context/1'),
    'C': (1, 'jev-1.13.0', 'ato.formation-generation-prompt/3', 'ato.formation-generation-context/2'),
    'oracle': (0, 'fixed', 'acceptance/1', 'ato.formation-generation-context/1'),
}
ARTIFACT_NAMES = ('requester', 'runtime', 'wasm')


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def require(condition, reason):
    if not condition:
        raise RuntimeError(reason + '; stop and register a prospective amendment')


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def verify_registration(plan, root=ROOT, plan_path=PLAN):
    """Verify committed bytes, not just the current worktree's self-reported hashes."""
    paths = list(plan['code_sha256'])
    required = {
        'scripts/acceptance/formation-generation-efficacy-e2.py',
        'scripts/acceptance/efficacy/e2_oracle.py',
        'scripts/acceptance/efficacy/e2_protocol.py',
        'scripts/acceptance/efficacy/e2_execution.py',
        'scripts/acceptance/efficacy/e2_fixtures.py',
        'scripts/acceptance/efficacy/e2_analyze.py',
        'scripts/acceptance/formation-generation-context.py',
        'scripts/acceptance/formation-typed-generation.py',
        'scripts/acceptance/formation-stage4-search.py',
        'scripts/acceptance/formation-5a-decision.py',
        'apps/formation-worker/examples/formation_search.rs',
    }
    require(required <= set(paths), 'missing registered code pins')
    for name, expected in plan['code_sha256'].items():
        require(sha256(root / name) == expected, 'registered code hash drift: ' + name)
    paths.append(str(plan_path.relative_to(root)))
    require(not git(root, 'status', '--porcelain', '--untracked-files=all', '--', *paths),
            'uncommitted registration/harness')
    for name in paths:
        committed = subprocess.check_output(['git', '-C', str(root), 'show', 'HEAD:' + name])
        require((root / name).read_bytes() == committed, 'uncommitted bytes: ' + name)
    require(json.loads(plan_path.read_text()) == plan, 'registration changed in memory')


def verify_api_pin(api_dir, plan):
    require(git(api_dir, 'rev-parse', '--show-toplevel') == str(api_dir.resolve()),
            'API must be the pinned checkout root')
    require(git(api_dir, 'rev-parse', 'HEAD') == plan['api_main'], 'API pin drift')
    # Include ignored files under source/config: git status alone can miss them.
    paths = ['src', 'scripts', 'migrations', 'package.json', 'pnpm-lock.yaml', 'wrangler.toml']
    require(not git(api_dir, 'status', '--porcelain', '--untracked-files=all', '--', *paths),
            'uncommitted API source/config')
    require(not git(api_dir, 'ls-files', '--others', '--ignored', '--exclude-standard', '--', *paths),
            'ignored API source/config')
    require(not git(api_dir, 'diff', 'HEAD', '--', *paths), 'API source drift')
    tracked = git(api_dir, 'ls-tree', '-r', 'HEAD', '--', *paths).splitlines()
    names, oids = [], []
    for entry in tracked:
        metadata, name = entry.split('\t', 1)
        mode, kind, oid = metadata.split()
        path = api_dir / name
        require(kind == 'blob' and mode in ('100644', '100755') and path.is_file() and not path.is_symlink(),
                'unsupported/missing API source: ' + name)
        names.append(name)
        oids.append(oid)
    actual = subprocess.check_output(['git', 'hash-object', '--no-filters', '--stdin-paths'],
                                     cwd=api_dir, input='\n'.join(names) + '\n', text=True).splitlines()
    require(actual == oids, 'API source bytes drift')
    return plan['api_main']


def verify_environment(plan, api_dir, requester, runtime):
    api_sha = verify_api_pin(api_dir, plan)
    paths = {'requester': requester, 'runtime': runtime,
             'wasm': api_dir / 'worker-final-bundle/receipt_authority.wasm'}
    hashes = {name: sha256(path) for name, path in paths.items()}
    for name in ARTIFACT_NAMES:
        require(hashes[name] == plan['artifacts'][name], 'artifact drift: ' + name)
    require((api_dir / 'initialized-0303').is_file(), 'local0303 not initialized')
    # Capture the actual launch inputs too. Oracle and primary must use the same
    # bundle/launcher bytes, even though no bundle is available at registration.
    launch_files = [api_dir / 'coordinator.mjs', *sorted((api_dir / 'worker-final-bundle').rglob('*'))]
    hashes['coordinator_tree'] = digest({str(p.relative_to(api_dir)): sha256(p)
                                      for p in launch_files if p.is_file()})
    require((api_dir / 'coordinator.mjs').is_file(), 'Coordinator launcher missing')
    return {'api_sha': api_sha, 'artifacts': hashes}


def registered_budget(plan):
    return dict(plan['budget'], max_attempts=plan['max_attempts'])


def arm_violations(record):
    identity = ARM_IDENTITY.get(record.get('arm'))
    if identity is None:
        return ['unregistered_arm']
    model_calls, model, prompt, schema = identity
    violations = []
    for field, expected in [('provider_calls', 1), ('model_calls', model_calls),
                            ('models', [model]), ('prompt_versions', [prompt])]:
        if record.get(field) != expected:
            violations.append(field + '_drift')
    if not isinstance(record.get('context'), dict) or record['context'].get('schema') != schema:
        violations.append('context_schema_drift')
    if model_calls:
        usage = record.get('usage')
        if (not isinstance(usage, list) or len(usage) != 1
                or not isinstance(usage[0], dict)
                or any(type(usage[0].get(k)) is not int or usage[0][k] < 0
                       for k in ('input_tokens', 'output_tokens'))):
            violations.append('missing_usage')
        if record.get('cost_usd') is None:
            violations.append('missing_cost')
    if record.get('generation_outcome') in ('provider_error', 'timeout'):
        violations.append('provider_' + record['generation_outcome'])
    if record.get('error') is not None:
        violations.append('harness_error')
    return violations


def generated_evidence(snapshot):
    """Link admission, execution and receipt without interpreting K in Python."""
    result = snapshot.get('result') or {}
    frozen = (result.get('search_state') or {}).get('frozen') or {}
    rows = snapshot.get('generation_rows') or []
    generated = snapshot.get('generated_derivation_ref')
    contract = result.get('contract_ref')
    if (len(rows) != 1 or rows[0].get('outcome') != 'admitted' or not generated
            or rows[0].get('derivation_ref') != generated or not contract
            or frozen.get('contract_ref') != contract
            or generated in [c.get('derivation_ref') for c in frozen.get('candidates', [])]):
        return None
    attempts = [a for a in result.get('attempts', []) if a.get('derivation_ref') == generated]
    if len(attempts) != 1:
        return None
    attempt = attempts[0]
    receipt = (attempt.get('formation_attempt') or {}).get('receipt') or {}
    execution = receipt.get('execution') or {}
    request_id = snapshot.get('satisfy_id')
    if (not attempt.get('attempt_id') or not request_id or result.get('satisfy_id') != request_id
            or execution.get('attempt_id') != attempt['attempt_id']
            or execution.get('request_id') != request_id
            or receipt.get('contract_ref') != contract or receipt.get('derivation_ref') != generated
            or type(receipt.get('fully_satisfied')) is not bool):
        return None
    return attempt, receipt


def same_k_success(snapshot, error=None):
    result = snapshot.get('result') or {}
    evidence = generated_evidence(snapshot)
    if (error is not None or snapshot.get('error') is not None or snapshot.get('requester_exit') != 0
            or result.get('status') != 'satisfied' or evidence is None):
        return False
    attempt, receipt = evidence
    routes = result.get('verified_routes') or []
    # The pinned requester exits 0 only after Rust accepts at least one route.
    # Requiring one route, for this D/attempt, makes that acceptance unambiguous.
    if len(routes) != 1:
        return False
    route = routes[0]
    return (attempt.get('status') == 'pass' and receipt['fully_satisfied'] is True
            and route.get('effective_contract_ref') == receipt['contract_ref']
            and route.get('derivation_ref') == receipt['derivation_ref']
            and route.get('attempt_id') == attempt['attempt_id']
            and bool(attempt.get('runtime_id')) and bool(attempt.get('environment_id'))
            and route.get('runtime_id') == attempt['runtime_id']
            and route.get('environment_id') == attempt['environment_id']
            and [r.get('receipt') for r in route.get('verifier_receipts', [])
                 if r.get('kind') == 'contract_verification'] == [receipt])


def safe_record(run, snapshot, case, permutation, arm, plan, error=None):
    result = snapshot.get('result') or {}
    attempts = result.get('attempts') or []
    rows = snapshot.get('generation_rows') or []
    frozen = (result.get('search_state') or {}).get('frozen') or {}
    policy = frozen.get('policy') or {}
    context = (snapshot.get('provider_request') or {}).get('state')
    outcome = rows[0].get('outcome') if len(rows) == 1 else None
    succeeded = same_k_success(snapshot, error)
    calls = snapshot.get('provider_call_count')
    if error is None:
        error = snapshot.get('error')
    record = dict(case=case, permutation=permutation, arm=arm, same_k_success=succeeded,
                  attempts=len(attempts), attempts_to_pass=len(attempts) if succeeded else None,
                  generation_outcome=outcome, admitted=outcome == 'admitted', declined=outcome == 'declined',
                  invalid_rejected=outcome in ('invalid', 'out_of_set', 'duplicate'),
                  draft_proposed=bool(rows and rows[0].get('draft_json')),
                  selected_entrypoint=snapshot.get('selected_entrypoint'),
                  generated_derivation_ref=snapshot.get('generated_derivation_ref'),
                  contract_ref=result.get('contract_ref'), fixture_hash=run.fixture_sha256,
                  context=context, context_hash=digest(context) if context else None,
                  provider_calls=calls, model_calls=calls if arm in ('B', 'C') else 0,
                  models=snapshot.get('models'), prompt_versions=snapshot.get('prompt_versions'),
                  usage=snapshot.get('usage'), cost_usd=snapshot.get('estimated_cost_usd') if arm in ('B', 'C') else 0,
                  provider_latency_ms=snapshot.get('provider_latency_ms'), elapsed_seconds=snapshot.get('elapsed_seconds'),
                  requester_exit=snapshot.get('requester_exit'), error=error,
                  budget=policy.get('budget'), generation_policy=policy.get('generation'),
                  evidence=snapshot, report_sha256=sha256(run.root / 'report.json') if (run.root / 'report.json').is_file() else None)
    violations = arm_violations(record)
    if not isinstance(snapshot.get('calls'), list) or len(snapshot['calls']) != 1:
        violations.append('missing_call_telemetry')
    if (not isinstance(snapshot.get('provider_latency_ms'), list)
            or len(snapshot['provider_latency_ms']) != 1
            or type(snapshot['provider_latency_ms'][0]) not in (int, float)
            or snapshot['provider_latency_ms'][0] < 0
            or type(snapshot.get('elapsed_seconds')) not in (int, float)):
        violations.append('missing_timing_telemetry')
    if outcome not in ('admitted', 'declined', 'invalid', 'out_of_set', 'duplicate', 'provider_error', 'timeout'):
        violations.append('missing_generation_outcome')
    if snapshot.get('read_errors'):
        violations.append('telemetry_read_error')
    if snapshot.get('requester_exit') != 0:
        violations.append('requester_error')
    if len(rows) != 1 or calls != 1:
        violations.append('missing_or_repeated_generation')
    if not record['contract_ref'] or frozen.get('contract_ref') != record['contract_ref']:
        violations.append('missing_or_changed_K')
    if record['budget'] != registered_budget(plan):
        violations.append('registered_budget_drift')
    generation = policy.get('generation') or {}
    if (generation.get('max_generations') != plan['max_generations']
            or generation.get('timeout_ms') != plan['generation_timeout_ms']):
        violations.append('generation_budget_drift')
    if generation.get('entrypoints') != plan['fixtures'][case]['permutations'][permutation]:
        violations.append('entrypoint_mapping_drift')
    if run.fixture_sha256 != digest(plan['fixtures'][case]['files']):
        violations.append('fixture_drift')
    exact = policy.get('runtime_constraint') or {}
    if (exact.get('kind') != 'exact' or not exact.get('runtime_id')
            or any(a.get('runtime_id') != exact['runtime_id'] for a in attempts)):
        violations.append('exact_escape')
    if any(a.get('status') == 'unknown' for a in attempts):
        violations.append('unknown_attempt')
    if context and CANARY in json.dumps(context):
        violations.append('canary_leak')
    if outcome == 'admitted':
        generated = snapshot.get('generated_derivation_ref')
        generated_attempts = [a for a in attempts if generated and a.get('derivation_ref') == generated]
        if len(generated_attempts) != 1 or rows[0].get('derivation_ref') != generated:
            violations.append('missing_generated_attempt')
    if result.get('status') == 'satisfied' and not succeeded:
        violations.append('unproven_generated_success')
    if result.get('status') not in ('satisfied', 'unsatisfied'):
        violations.append('unsettled_result')
    for call in snapshot.get('calls') or []:
        if call.get('submission_accepted') is not True:
            violations.append('generation_submission_failed')
        reason = (call.get('answer') or {}).get('fallback')
        if reason in ('provider_error', 'timeout'):
            violations.append(reason)
    for attempt in attempts:
        attestation = attempt.get('attestation') or {}
        if attestation.get('execution_started') is not True or attestation.get('attempt_record') != 'finished':
            violations.append('environment_or_execution_failure')
        failure = attempt.get('failure') or {}
        if attempt.get('status') == 'fail' and failure.get('code') not in (
                'http_status_mismatch', 'http_body_digest_mismatch', 'candidate_not_observable'):
            violations.append('unexpected_runtime_failure')
        receipt = (attempt.get('formation_attempt') or {}).get('receipt')
        if not receipt and failure.get('code') != 'candidate_not_observable':
            violations.append('missing_runtime_receipt')
        if receipt:
            execution = receipt.get('execution') or {}
            if (not attempt.get('attempt_id') or not snapshot.get('satisfy_id')
                    or execution.get('attempt_id') != attempt['attempt_id']
                    or execution.get('request_id') != snapshot['satisfy_id']):
                violations.append('receipt_not_fresh')
        if receipt and (receipt.get('contract_ref') != record['contract_ref']
                        or receipt.get('derivation_ref') != attempt.get('derivation_ref')):
            violations.append('receipt_K_D_drift')
    record['violations'] = sorted(set(violations))
    return record


def oracle_key(cell):
    return f"{cell['case']}p{cell['permutation']}:{cell['provider']}"


def validate_oracle_cells(plan):
    cells = plan['oracle']['cells']
    expected = {f'{case}p0:fixed:{entry}' for case in plan['fixtures'] for entry in ('k4', 'v9')}
    keys = [oracle_key(cell) for cell in cells]
    require(len(keys) == 24 and len(set(keys)) == 24 and set(keys) == expected, 'oracle cell set drift')
    for cell in cells:
        require(cell['provider'] == 'fixed:' + cell['draft_entrypoint_id']
                and cell['mapped_file'] == plan['fixtures'][cell['case']]['permutations'][0][cell['draft_entrypoint_id']]
                and type(cell['expected_fully_satisfied']) is bool, 'oracle cell definition drift')
    return cells


def oracle_expectation(record, cell):
    evidence = generated_evidence(record.get('evidence') or {})
    if (record.get('error') is not None or record.get('violations') or evidence is None
            or record.get('requester_exit') != 0 or record.get('model_calls') != 0
            or record.get('selected_entrypoint') != cell['draft_entrypoint_id']):
        return False
    attempt, receipt = evidence
    if cell['expected_fully_satisfied']:
        return same_k_success(record['evidence'])
    return (receipt['fully_satisfied'] is False and attempt.get('status') == 'fail'
            and record['evidence']['result'].get('status') == 'unsatisfied'
            and not record['evidence']['result'].get('verified_routes'))


def validate_oracle_result(path, plan, plan_sha, environment):
    cells = validate_oracle_cells(plan)
    require(path.is_file(), 'oracle result missing')
    output = json.loads(path.read_text())
    require(output.get('plan_sha256') == plan_sha, 'oracle registration SHA mismatch')
    require(output.get('environment') == environment, 'oracle artifact/API pins mismatch')
    require(output.get('error') is None, 'oracle error')
    require(output.get('complete') is True and output.get('all_expectations_match') is True
            and output.get('model_calls') == 0, 'oracle incomplete/failed/model call')
    rows = output.get('results') or []
    keys = [r.get('cell_key') for r in rows]
    require(len(keys) == 24 and len(set(keys)) == 24
            and set(keys) == {oracle_key(c) for c in cells}, 'oracle result cell set mismatch')
    by_key = {r['cell_key']: r for r in rows}
    for cell in cells:
        row = by_key[oracle_key(cell)]
        # Recompute from persisted evidence; do not trust summary booleans.
        record = row['record']
        require(record.get('case') == cell['case'] and record.get('permutation') == 0
                and record.get('arm') == 'oracle' and record.get('model_calls') == 0
                and not record.get('violations') and row.get('expectation_matches') is True,
                'oracle record identity/violation mismatch')
        rebuilt = safe_record(
            type('Run', (), {'fixture_sha256': record.get('fixture_hash'), 'root': path.parent / 'unused'})(),
            record.get('evidence') or {}, cell['case'], 0, 'oracle', plan, record.get('error'))
        require(row.get('expected_fully_satisfied') is cell['expected_fully_satisfied']
                and oracle_expectation(rebuilt, cell), 'oracle expectation/evidence mismatch')
    return output


def write_json(path, value, exclusive=False):
    with path.open('x' if exclusive else 'w') as output:
        json.dump(value, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
