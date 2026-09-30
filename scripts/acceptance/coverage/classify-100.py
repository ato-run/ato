#!/usr/bin/env python3
"""6b-E reporting rules. Fixed before any current-wave Formation results."""
PRIMARY_CLASSES = [
    'success', 'known-D/authoring', 'build-capability', 'runtime/toolchain',
    'adapter', 'multi-service', 'dependency/network', 'verifier', 'browser/ui',
    'retention/replay', 'effect/policy', 'unknown',
]
STAGE_ORDER = {'source': 0, 'preset': 1, 'authoring': 1, 'projection': 2,
               'plan': 2, 'admission': 3, 'record': 3, 'build': 4,
               'execution': 4, 'run': 4, 'realization': 4,
               'verification': 5, 'browser': 5, 'publication': 6, 'cleanup': 6}

def primary_of(code, stage):
    if code == 'network_denied' or code.startswith(('effect_', 'policy_', 'runtime_policy_', 'authorization_')):
        return 'effect/policy'
    if stage == 'source' or code.startswith('source_'):
        return 'adapter'
    if code.startswith(('browser_', 'ui_')) or stage == 'browser':
        return 'browser/ui'
    if code.startswith(('retention_', 'replay_', 'attempt_record_', 'candidate_cleanup_')) or stage in ('publication', 'cleanup', 'record'):
        return 'retention/replay'
    if code.startswith(('multi_service_', 'service_group_')):
        return 'multi-service'
    if code.startswith(('dependency_', 'registry_', 'package_download_', 'network_')):
        return 'dependency/network'
    if code.startswith(('intent_unsupported', 'package_manager_version', 'toolchain', 'runtime_cannot', 'runtime_unavailable')):
        return 'runtime/toolchain'
    if code.startswith(('preset_', 'capsule_toml', 'authoring_', 'workspace_')) or code in (
            'intent_requires_authoring', 'intent_no_lane', 'intent_ambiguous_lockfiles', 'intent_malformed'):
        return 'known-D/authoring'
    if stage in ('preset', 'authoring', 'projection', 'plan'):
        return 'known-D/authoring'
    if stage in ('build', 'execution', 'run', 'realization'):
        return 'build-capability'
    if stage == 'verification':
        return 'verifier'
    return 'unknown'

def classify(observation, process, journals=()):
    layers = dict.fromkeys('ABCDEFGHI', False)
    result = (observation or {}).get('result', {})
    if 'timeout_seconds' in process:
        return 'unknown', 'hard_timeout', layers
    if result.get('status') == 'typed_terminal_error':
        return primary_of(result['code'], result.get('stage')), result['code'], layers
    if result.get('status') != 'formation_result':
        return 'unknown', 'untyped_error', layers
    layers['A'] = True  # local::run passed archive/tree normalization.
    formed = result['result']
    attempts = formed.get('attempts', [])
    started = {row['attempt_id'] for row in journals if row.get('identity') and row.get('state') in ('started', 'finished')}
    for attempt in attempts:
        layers['B'] |= bool(attempt.get('contract_ref'))
        layers['C'] |= bool(attempt.get('derivation_ref'))
        layers['D'] |= attempt.get('attempt_id') in started or attempt.get('status') in ('failed', 'verified')
        layers['E'] |= bool(attempt.get('realization'))
        layers['F'] |= bool(attempt.get('verification')) or bool(attempt.get('receipt'))
        receipt = attempt.get('receipt') or {}
        fresh = ((receipt.get('execution') or {}).get('attempt_id') == attempt.get('attempt_id')
                 and bool(attempt.get('attempt_id')))
        layers['G'] |= (receipt.get('fully_satisfied') is True and fresh
                        and receipt.get('contract_ref') == attempt.get('contract_ref')
                        and receipt.get('derivation_ref') == attempt.get('derivation_ref'))
    layers['H'] = any(route.get('materialization_ref') for route in formed.get('verified_routes', []))
    # I is unmeasured in this wave; the separate historical functional append
    # is referenced by the report, never counted as this wave's acceptance.
    if layers['G']:
        return 'success', 'fully_satisfied', layers
    best = max(attempts, key=lambda attempt: STAGE_ORDER.get((attempt.get('failure') or {}).get('stage'), 0), default={})
    failure = best.get('failure') or {}
    if not failure:
        return 'unknown', 'no_attempt_failure', layers
    return primary_of(failure['code'], failure.get('stage')), failure['code'], layers
