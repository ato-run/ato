#!/usr/bin/env python3
"""Offline ledger from digest-verified session artifacts, never an executor."""
import argparse
import base64
import hashlib
import json
from pathlib import Path


def read(path):
    return json.loads(Path(path).read_text())


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def case(root, app):
    cell = root / 'codex/cells' / f'{app["index"]:03}'
    summaries = sorted(cell.glob('summary*.json'))
    final = [read(p) for p in summaries if read(p).get('rounds_consumed') is not None]
    assert len(final) == 1, 'ambiguous final result'
    summary = final[0]
    result_path = root / 'codex' / summary['result']
    assert sha(result_path) == summary['result_sha256']
    result = read(result_path)
    assert result['contract_ref'] == app['contract_ref'], 'K changed'
    assert result['approval'] == 'not_assessed' and not result['deployed']
    assert not result['candidate_producer_calls'], 'this append is session-only'
    assert result['effective_max_rounds'] == 3
    # These two actual cases did not reach admission or Runtime. Do not infer
    # an execution/receipt from Runtime registration or offline validation.
    assert not result['attempts'] and result['submission'] is None
    assert result['known_attempts'] == result['generated_attempts'] == 0
    reason = cell / 'producer.reasoning'
    records = []
    for path in sorted(reason.glob('r*_s*.record.json')):
        record = read(path)
        stem = path.name.removesuffix('.record.json')
        input_path, response_path = reason / (stem + '.input.json'), reason / (stem + '.response.json')
        input_value, response = read(input_path), read(response_path)
        assert record['input_sha256'] == 'sha256:' + sha(input_path)
        assert response['input_sha256'] == record['input_sha256']
        assert input_value['frozen_contract_ref'] == app['contract_ref']
        assert record['provider_call'] is None, 'API call in session append'
        output = base64.b64decode(record['raw_output_base64'], validate=True)
        assert record['output_sha256'] == 'sha256:' + hashlib.sha256(output).hexdigest()
        proposal = json.loads(output)
        records.append({'input_file': str(input_path.relative_to(root)),
            'input_sha256': record['input_sha256'],
            'record_file': str(path.relative_to(root)), 'record_sha256': sha(path),
            'output_sha256': record['output_sha256'],
            'round_seq': int(stem[1:4]), 'exchange_seq': int(stem[6:9]),
            'output_kinds': [p['kind'] for p in proposal['proposals']],
            'decline_reasons': [p.get('reason') for p in proposal['proposals'] if p['kind'] == 'unsupported'],
            'inspected': record['inspected'], 'inspected_bytes': record['inspected_bytes'],
            'inspection_error': record['inspection_error'],
            'bridge_active_elapsed_ms': record['elapsed_ms'],
            'rounds_remaining': input_value['rounds_remaining'],
            'projection': input_value['projection']})
    assert len(records) == result['reasoning_accounting']['call_count']
    inputs = sorted(reason.glob('r*_s*.input.json'))
    responses = sorted(reason.glob('r*_s*.response.json'))
    accepted = {Path(r['input_file']).name.removesuffix('.input.json') for r in records}
    unaccepted = []
    for p in inputs:
        stem = p.name.removesuffix('.input.json')
        if stem in accepted:
            continue
        response = reason / (stem + '.response.json')
        unaccepted.append({'input_file': str(p.relative_to(root)), 'input_sha256': sha(p),
            'response_delivered': response.exists(),
            'response_sha256': sha(response) if response.exists() else None,
            'classification': 'late staged response, no accepted record' if response.exists()
                              else 'input saved, no response delivered before deadline'})
    return {'index': app['index'], 'name': app['name'], 'source': app,
        'provider': 'codex_session', 'search_id': result['search_id'],
        'contract_ref': result['contract_ref'], 'known_D': False,
        'search_status': result['search_status'], 'stop': result['stop'],
        'rounds_consumed': result['rounds_consumed'], 'round_timing': result['rounds'],
        'attempts': 0, 'generated_D_accepted_in_search': 0,
        'source_programs_executed': 0, 'fresh_receipts': 0, 'typed_K_pass': False,
        'permission_reduction': result['permission_reduction'],
        'input_files': len(inputs), 'response_files': len(responses),
        'accepted_session_exchanges': len(records), 'unaccepted_exchanges': unaccepted,
        'records': records, 'proposal_evidence': result['proposal_evidence'],
        'API_calls': 0, 'DecisionProvider_calls': 0,
        'session_tokens': None, 'session_cost_usd_micros': None,
        'case_elapsed_segments_seconds': [read(p)['elapsed_seconds'] for p in summaries],
        'elapsed_limitations': 'segments omit stopped/restart intervals; bridge active time is not total Codex inference latency',
        'result_sha256': sha(result_path), 'functional_acceptance': 'not_measured',
        'persistence': 'not_measured', 'approval': 'not_assessed', 'deployed': False}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', required=True)
    parser.add_argument('--manifest', required=True)
    parser.add_argument('--selection', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    root = Path(args.root)
    manifest = read(args.manifest)
    for file in manifest['files']:
        path = root / file['path']
        assert path.resolve().is_relative_to(root.resolve()), 'escaped evidence path'
        assert path.stat().st_size == file['bytes'] and sha(path) == file['sha256']
    cases = [case(root, app) for app in read(args.selection)['applications']]
    offline = read(root / 'offline-review/svgomg-offline-canonical.json')
    assert not offline['submitted'] and not offline['typed_K_pass']
    assert not offline['admitted_to_runtime'] and offline['fresh_receipt'] is None
    assert offline['source_programs_executed'] == 0 and len(offline['compiled']) == 1
    ledger = {'schema': 'ato.formation-shared-reasoning-ledger/1',
        'scope': 'Codex-first two-OSS prototype append; not 100-app remeasurement',
        'implemented': True, 'acceptance_complete': False, 'merged': False,
        'deployed': False, 'remote_migration': False,
        'pins': {'initial_CLI_and_worker': 'fb140026bb555d01f5446cd1a92f04affac4f9b6',
                 'corrected_CLI_and_worker': '688eb4a6484bf8e0237c6265a64aa0d590b16488',
                 'Rust_receipt_authority_WASM': 'fb140026bb555d01f5446cd1a92f04affac4f9b6',
                 'API_JS': '4ff88c038e095951e2277b759ac7ae012148e02e'},
        'same_code_qualification': False,
        'infrastructure_correction': 'SVGOMG input catalog duplicated inventory and hid scripts. Code changed only during stopped infra repair; same Search counters/deadline remained. Both app terminals are non-PASS.',
        'cases': cases,
        'offline_review': {'path': 'offline-review/svgomg-offline-canonical.json',
                          'sha256': sha(root / 'offline-review/svgomg-offline-canonical.json'),
                          'contract_ref': offline['contract_ref'],
                          'derivation_ref': offline['compiled'][0]['canonical']['derivation_ref'],
                          'validation': 'accepted', 'executed': False, 'submitted': False,
                          'fresh_receipt': None, 'typed_K_pass': False},
        'totals': {'Codex_unique_PASS_apps': 0, 'API_unique_PASS_apps': 0,
                   'first_D_PASS': 0, 'repair_PASS': 0,
                   'rounds_consumed': sum(c['rounds_consumed'] for c in cases),
                   'input_files': sum(c['input_files'] for c in cases),
                   'response_files': sum(c['response_files'] for c in cases),
                   'accepted_session_exchanges': sum(c['accepted_session_exchanges'] for c in cases),
                   'inspected_source_files': sum(len(r['inspected']) for c in cases for r in c['records']),
                   'inspected_source_bytes': sum(r['inspected_bytes'] for c in cases for r in c['records']),
                   'valid_generated_D_in_search': 0, 'source_programs_executed': 0,
                   'API_calls': 0, 'DecisionProvider_calls': 0,
                   'API_actual_cost_usd_micros': 0, 'API_reservation_consumed_usd_micros': 0,
                   'API_remaining_usd_micros': 564906, 'unresolved_API_reservations': 0,
                   'Codex_session_tokens': None, 'Codex_session_cost_usd_micros': None,
                   'cost_per_additional_PASS': None},
        'API_gate': {'executed': False, 'reason': 'Codex real OSS fresh same-K PASS gate not satisfied',
                     'max_reservation_usd_micros': 117972, 'budget_sufficient': True},
        'historical': {'original_100_baseline_PASS': 7, 'original_100_exploration_PASS': 7,
                       'original_additional_PASS': 0, 'ledgers_overwritten': False,
                       'prior_API_SVGOMG_PASS': 'separate historical repair append, not Codex-first evidence'},
        'residual_gaps': [
            'SVGOMG infra correction investigation exhausted all three rounds with no D attempt; independent new acceptance requires user reply to preserve no-reset constraint.',
            'changedetection.io unhashed/ranged Python dependencies cannot be lowered through current hash-pinned binary-wheel operation; no bound OCI builder recipe.',
            'Projection records omission and retains acquired files but may still truncate a manifest/new entrypoint; initial Python entrypoint heuristic picked library App.py.',
            'Reasoned decline after inspection was followed by two identical declines before no_progress; no additional source evidence or D execution.',
            'Codex fresh PASS, evidence-repair PASS, permission reduction receipt and independent live API gate are not established in this append.'
        ],
        'raw_manifest_sha256': sha(args.manifest)}
    Path(args.output).write_text(json.dumps(ledger, indent=2) + '\n')
    print(json.dumps(ledger['totals']))


if __name__ == '__main__':
    main()
