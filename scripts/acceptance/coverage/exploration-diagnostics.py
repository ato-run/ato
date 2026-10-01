#!/usr/bin/env python3
"""Offline analysis of frozen exploration evidence; never execute or score apps.

The original journal persists request digests/lengths, not request bodies. Source
novelty below concerns *requested* verified refs, not proof of provider-visible
text. Do not infer model/toolchain responsibility from an unexplained decline.
"""
import argparse
import base64
from collections import Counter
import hashlib
import json
from pathlib import Path


def read(path):
    return json.loads(path.read_text())


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def plans(batch):
    return [op['plan'] for p in batch.get('proposals', [])
            for op in p.get('operations', []) if op.get('operation') == 'execution_plan@1']


def requirements(plan, kind):
    return {json.dumps(r, sort_keys=True) for r in plan.get('requirements', {}).get(kind, [])}


def semantic(batch):
    # Explanatory prose does not change executable D. Canonical D refs below
    # remain authoritative; this hash also compares rejected proposals.
    copy = json.loads(json.dumps(batch))
    for plan in plans(copy):
        plan.pop('basis', None)
        plan.pop('unknowns', None)
        for kind in ('network', 'authority'):
            req = plan.get('requirements', {}).get(kind)
            if req is not None:
                plan['requirements'][kind] = sorted(req, key=lambda r: json.dumps(r, sort_keys=True))
    return digest(copy)


def disposition(round_, parsed):
    outcomes = round_.get('outcomes', [])
    if any(o['status'] == 'admitted' for o in outcomes):
        return 'valid_D'
    if any(o['status'] == 'unsupported' for o in outcomes):
        return 'unexplained_decline'
    codes = [o.get('code') for o in outcomes if o.get('code')]
    if codes:
        return codes[0]
    if round_.get('raw_output_base64') and parsed is None:
        return 'invalid_JSON'
    if round_.get('error_class') == 'invalid_output' and parsed is not None:
        return 'typed_schema_violation'
    return 'provider_' + (round_.get('error_class') or round_['status'])


def attempt_evidence(attempt):
    facts, denied = [], {'network': [], 'authority': []}
    for evidence in attempt.get('exploration_evidence') or []:
        if evidence['kind'] == 'exploration_execution_facts':
            facts.extend(evidence['facts'])
        if evidence['kind'] == 'exploration_authority_evidence':
            denied['authority'].extend(evidence['refused'])
        if evidence['kind'] == 'exploration_network_evidence':
            for report in evidence['reports']:
                for refusal in report['report']['refused']:
                    denied['network'].append({'phase': report['phase'], 'host': refusal['target'], 'port': refusal['port']})
    return {'attempt_id': attempt['attempt_id'], 'derivation_ref': attempt['derivation_ref'],
            'failure': attempt.get('failure'), 'status': attempt['status'],
            'execution_started': bool((attempt.get('attestation') or {}).get('execution_started')),
            'facts': facts, 'denials': denied}


def analyze(ledger, root):
    applications, round_counts = [], Counter()
    progress_counts, provider_errors = Counter(), Counter()
    for app in ledger['applications']:
        index = app['index']
        cell = root / 'cells' / f'{index:03d}'
        if not (cell / 'status.json').exists():
            applications.append({'index': index, 'name': app['name'], 'called': False,
                                 'rounds': [], 'valid_D': False, 'executed': False,
                                 'source_terminal': app['adaptive_terminal']})
            continue
        state = read(cell / 'status.json')
        attempts = [attempt_evidence(a) for a in state['attempts']]
        rounds = state.get('proposal_history', []) + ([state['proposal_round']] if state.get('proposal_round') else [])
        pre = read(cell / 'preflight.json')['preregistration']
        seen_refs = {e['logical_id'] for e in pre['source_context']}
        requested_before, hashes, refs = set(), set(), set()
        previous_codes, previous_plan = set(), None
        analyses = []
        journal = [json.loads(line) for line in (cell / 'producer.jsonl').read_text().splitlines()]
        requests = [v['request'] for v in journal if 'request' in v]
        for round_ in rounds:
            encoded = round_.get('raw_output_base64')
            parsed = None
            if encoded:
                raw = base64.b64decode(encoded)
                assert round_['raw_output_digest'] == 'sha256:' + hashlib.sha256(raw).hexdigest()
                try:
                    parsed = json.loads(raw)
                except json.JSONDecodeError:
                    pass
            category = disposition(round_, parsed)
            round_counts[category] += 1
            if round_.get('error_class'):
                provider_errors[round_['error_class']] += 1
            batch = parsed if isinstance(parsed, dict) else {}
            inspected = [r['file_id'] for p in batch.get('proposals', [])
                         if p.get('kind') == 'inspect_source' for r in p.get('sources', [])]
            current = plans(batch)
            admitted = sorted({o['derivation_ref'] for o in round_.get('outcomes', [])
                               if o['status'] == 'admitted'})
            codes = {o['code'] for o in round_.get('outcomes', []) if 'code' in o}
            matched = [a for a in attempts if a['derivation_ref'] in admitted and a['derivation_ref'] not in refs]
            plan_delta = []
            if current and previous_plan is not None:
                plan_delta = sorted(k for k in set(current[0]) | set(previous_plan)
                                    if current[0].get(k) != previous_plan.get(k) and k not in ('basis', 'unknowns'))
            sem = semantic(batch) if parsed is not None else None
            progress = {'new_inspection_refs_requested': sorted(set(inspected) - seen_refs - requested_before),
                        'repeated_inspection_refs_requested': sorted(set(inspected) & (seen_refs | requested_before)),
                        'repeated_semantic_proposal': sem is not None and sem in hashes,
                        'repeated_canonical_D': bool(admitted) and set(admitted) <= refs,
                        'execution_fields_changed': plan_delta,
                        'preceding_validator_errors_absent': sorted(previous_codes - codes),
                        'validator_repair_to_valid_D': bool(previous_codes and admitted)}
            for key in ('repeated_semantic_proposal', 'repeated_canonical_D', 'validator_repair_to_valid_D'):
                progress_counts[key] += int(progress[key])
            progress_counts['rounds_requesting_new_inspection_refs'] += bool(progress['new_inspection_refs_requested'])
            progress_counts['rounds_requesting_repeated_inspection_refs'] += bool(progress['repeated_inspection_refs_requested'])
            progress_counts['rounds_with_changed_execution_fields'] += bool(plan_delta)
            analyses.append({'round': round_['round_seq'], 'disposition': category,
                             'provider_call_present': bool(round_.get('provider_call')),
                             'raw_output_digest': round_.get('raw_output_digest'),
                             'semantic_proposal_sha256': sem, 'outcomes': [{k: o[k] for k in ('status', 'code', 'derivation_ref') if k in o}
                                                                         for o in round_.get('outcomes', [])],
                             'plans': current, 'inspection_refs': inspected,
                             'progress_observations': progress, 'new_attempts': matched})
            if sem:
                hashes.add(sem)
            refs.update(admitted)
            requested_before.update(inspected)
            previous_codes = codes - {'source_inspection_requested'}
            if current:
                previous_plan = current[0]
        called = bool(app['producer_calls'])
        valid = bool(app['generated_D_refs'])
        assert refs == set(app['generated_D_refs'])
        applications.append({'index': index, 'name': app['name'], 'primary_language': app['primary_language'],
                             'called': called, 'valid_D': valid, 'executed': bool(app['generated_D_executed']),
                             'last_observed_disposition': analyses[-1]['disposition'] if analyses else 'no_call',
                             'rounds': analyses, 'provider_request_digest_evidence': requests,
                             'initial_context_text_bytes': pre['source_text_bytes'],
                             'initial_context_entries': [{'logical_id': e['logical_id'], 'truncated': e['truncated'],
                                                          'text_bytes': len(e['text'].encode())} for e in pre['source_context']],
                             'status_sha256': sha(cell / 'status.json'),
                             'producer_journal_sha256': sha(cell / 'producer.jsonl'),
                             'attempts': attempts})
    no_valid = [a for a in applications if a['called'] and not a['valid_D']]
    funnels = {}
    for kind in ('network', 'authority'):
        detected, proposed, valid, rerun, recovered = set(), set(), set(), set(), set()
        for app in applications:
            denial_set = set()
            for r in app['rounds']:
                if denial_set and any(requirements(p, kind) & denial_set for p in r['plans']):
                    proposed.add(app['index'])
                    if any(o['status'] == 'admitted' for o in r['outcomes']):
                        valid.add(app['index'])
                    if any(a['execution_started'] for a in r['new_attempts']):
                        rerun.add(app['index'])
                    if any(a['status'] == 'pass' for a in r['new_attempts']):
                        recovered.add(app['index'])
                for a in r['new_attempts']:
                    for req in a['denials'][kind]:
                        denial_set.add(json.dumps(req, sort_keys=True))
            if denial_set:
                detected.add(app['index'])
        funnels[kind] = {name: {'count': len(values), 'indices': sorted(values)}
                         for name, values in [('shortage_detected', detected), ('matching_change_proposed', proposed),
                                              ('valid_D', valid), ('reexecuted', rerun), ('same_K_recovered', recovered)]}
    return {'schema': 'ato.formation-exploration-diagnostics/1',
            'original_ledger_sha256': digest(ledger),
            'runtime_pin': ledger['ato_pin'], 'api_pin': ledger['ato_api_pin'],
            'totals': {'apps': len(applications), 'called': sum(a['called'] for a in applications),
                       'no_valid_D': len(no_valid), 'valid_D_unexecuted': sum(a['valid_D'] and not a['executed'] for a in applications),
                       'generated_D_executed_apps': sum(a['executed'] for a in applications),
                       'rounds': sum(round_counts.values())},
            'round_dispositions': dict(sorted(round_counts.items())),
            'no_valid_D_last_observed_disposition': dict(sorted(Counter(a['last_observed_disposition'] for a in no_valid).items())),
            'progress_observations': dict(sorted(progress_counts.items())),
            'provider_error_class_distribution': dict(provider_errors),
            'permission_recovery_funnels': funnels,
            'limitations': ['Original request evidence contains digests and byte lengths, not provider-sent request bodies.',
                           'Requested source novelty does not establish new provider-visible text; no source-text progress count is asserted.',
                           'Unsupported has no reason field in this wave; unexplained declines cannot be attributed to model or Ato capability.',
                           'A disappearing validator code is an observed change, not proof of repair unless the next D is valid.',
                           'Permission recovery means same-K PASS; execution reach is separately counted.'],
            'applications': applications, 'model_calls_added': 0, 'merged': False, 'deployed': False}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--ledger', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    manifest = read(args.manifest)
    for entry in manifest:
        path = args.root / entry['path']
        assert path.stat().st_size == entry['bytes'] and sha(path) == entry['sha256'], entry['path']
    result = analyze(read(args.ledger), args.root)
    result['original_ledger_file_sha256'] = sha(args.ledger)
    result['verified_raw_manifest_sha256'] = sha(args.manifest)
    result['raw_files_verified'] = len(manifest)
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps({k: v for k, v in result.items() if k not in ('applications',)}))


if __name__ == '__main__':
    main()
