#!/usr/bin/env python3
"""Immutable D3 v2 evidence contract. Offline only; never reads credentials.

The original plan remains byte-identical. Request/body hashes are observations,
not preclaim predictions. Controllers must also run the original A–M barriers.
"""
import argparse
import copy
import importlib.util
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('d3_v1', HERE / 'deepseek-live.py')
v1 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v1)
V1_SHA = 'b67607c6cff1de5339f79822bd3e5d6610d1663dcce6e3f0d6b418ef8a2a1585'
SCHEMA = 'ato.formation-deepseek-d3-plan/2'
REASON = 'post-claim dynamic timeout required in-process exact request evidence'
FILES = {
    'capture': 'apps/formation-worker/src/runtime_network/proposal/deepseek.rs',
    'journal': 'apps/formation-worker/src/runtime_network/proposal/budget.rs',
    'timeout': 'apps/formation-worker/src/runtime_network/proposal.rs',
    'helper': 'apps/formation-worker/examples/proposal_budget.rs',
}
CONTRACT = {
    'proposal_request_sha256': 'required_runtime_observation',
    'provider_body_sha256': 'required_runtime_observation',
    'capture_point': 'after_final_timeout_before_credential_read',
    'durable_before_send': True,
    'hash_algorithm': 'SHA-256_lowercase_sha256_prefix',
    'proposal_serialization': 'serde_jcs::to_string(ProposalRequestV2)_UTF8',
    'provider_serialization': 'serde_json::to_vec(body)_once_same_Vec_to_reqwest_body',
    'journal_event': {'request': {'cell': 'string', 'proposal_request_sha256': 'sha256',
        'provider_body_sha256': 'sha256', 'timeout_ms': 'u64',
        'proposal_request_bytes': 'u64_length_only', 'provider_body_bytes': 'u64_length_only'}},
    'reservation': 'one_Request_event_append_fsync_before_credential_read_no_refund',
    'timeout_formula': {
        'remaining_round_ms': 'expires_at_ms.saturating_sub(now_ms_after_claim)',
        'request_timeout_ms': 'min(remaining_round_ms, owner_policy_timeout_ms) - completion_reserve_ms',
        'completion_reserve_ms': 500,
        'require': 'min(remaining_round_ms, owner_policy_timeout_ms) > completion_reserve_ms',
        'http_timeout_ms': 'min(adapter_config_timeout_ms, request_timeout_ms)',
        'observation': 'request.remaining_budget.timeout_ms',
        'bounds_ms': [1, 30000],
        'registered_equivalence': 'round<=30000; owner_policy=adapter_config=30000; request=http=remaining_round_ms-500',
    },
    'inspection': 'proposal_budget inspect-request <BudgetPlan.json> <journal> <search_id>',
    'no_body_persistence': True,
    'credential_headers_excluded': True,
}


def contract(root):
    return {**copy.deepcopy(CONTRACT), 'implementation_files': {
        name: {'path': path, 'sha256': v1.sha((root / path).read_bytes())}
        for name, path in FILES.items()}}


def original(path):
    v1.require(v1.sha(path.read_bytes()) == V1_SHA, 'v1 immutable plan SHA')
    plan = v1.load(path)
    v1.check_plan(plan)
    return plan


def check(plan, old, root):
    """N15–N19 in addition to original A–M; no key, journal or model HTTP."""
    v1.require(plan['schema'] == SCHEMA, 'v2 schema drift')
    v1.require(plan['supersedes'] == {'plan_sha256': V1_SHA, 'reason': REASON}, 'supersedes drift')
    # All unlisted fields, including every fixture/K/catalog/context byte hash,
    # must equal the historical plan. Even an otherwise valid change is rejected.
    before = copy.deepcopy(plan)
    before.pop('supersedes'); before.pop('request_evidence')
    before['schema'] = old['schema']; before['ato_sha'] = old['ato_sha']
    v1.require(before == old, 'non-evidence preregistration drift')
    v1.clean_pin(root, plan['ato_sha'])
    v1.require(plan['request_evidence'] == contract(root), 'request evidence implementation/schema/formula/helper/serializer drift')


def generate(old_path, root, output):
    old = original(old_path)
    plan = copy.deepcopy(old)
    plan.update(schema=SCHEMA, ato_sha=v1.git(root, 'rev-parse', 'HEAD'),
                supersedes={'plan_sha256': V1_SHA, 'reason': REASON}, request_evidence=contract(root))
    check(plan, old, root)
    with output.open('x') as stream:
        json.dump(plan, stream, indent=2, ensure_ascii=False)
        stream.write('\n')
    return plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['generate', 'check'])
    parser.add_argument('--v1', type=pathlib.Path, required=True)
    parser.add_argument('--root', type=pathlib.Path, required=True)
    parser.add_argument('--plan', type=pathlib.Path, required=True)
    parser.add_argument('--sha256')
    args = parser.parse_args()
    if args.mode == 'generate':
        generate(args.v1, args.root, args.plan)
    else:
        v1.require(v1.sha(args.plan.read_bytes()) == args.sha256, 'v2 plan SHA drift')
        check(v1.load(args.plan), original(args.v1), args.root)
    print(json.dumps({'plan_sha256': v1.sha(args.plan.read_bytes()), 'live_calls': 0, 'key_read': False}))


if __name__ == '__main__':
    main()
