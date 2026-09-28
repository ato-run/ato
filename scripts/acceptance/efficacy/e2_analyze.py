"""Analyze all preregistered E2 cells; never discards negative/missing outcomes.

Completeness means the recorded cell-key set equals the exact preregistered
set from docs/ops/formation-efficacy-e2-plan.json. Count-only matching,
duplicate cells, unknown cases/permutations/arms and unfinished ledgers can
never close the efficacy gate.
"""
import json
try:
    from .e2_protocol import arm_violations, same_k_success
except ImportError:
    from e2_protocol import arm_violations, same_k_success
from collections import Counter
import statistics
import sys
from pathlib import Path

# Immutable E2 domain; the unit test pins it to the preregistration exactly.
REGISTERED_CASES = (
    'C01', 'C02', 'C03', 'C04',
    'D01', 'D02', 'D03',
    'L01', 'L02',
    'P01', 'P02', 'P03',
)
ARMS = 'ABC'
CELLS_PER_ARM = 24
REGISTERED_CELLS = frozenset(
    (case, permutation, arm)
    for case in REGISTERED_CASES
    for permutation in (0, 1)
    for arm in ARMS
)


def summarize(rows):
    # Independently enforce telemetry even if a hand-edited row cleared violations.
    rows = [dict(r, violations=list(r.get('violations') or []) + arm_violations(r)) for r in rows]
    for row in rows:
        if row.get('same_k_success') and not same_k_success(row.get('evidence') or {}, row.get('error')):
            row['same_k_success'] = False
            row['violations'].append('unproven_generated_success')
    keys = [(r['case'], r['permutation'], r['arm']) for r in rows]
    if len(set(keys)) != len(keys):
        raise ValueError('duplicate cell')
    totals = {}
    for arm in ARMS:
        subset = [r for r in rows if r['arm'] == arm]
        usage = [u for r in subset for u in (r.get('usage') or []) if isinstance(u, dict)]
        drafts = sum(r['draft_proposed'] for r in subset)
        latencies = [ms for r in subset for ms in (r.get('provider_latency_ms') or []) if ms is not None]
        timings = [r['elapsed_seconds'] for r in subset if r.get('elapsed_seconds') is not None]
        costs = [r['cost_usd'] for r in subset if r.get('cost_usd') is not None]
        totals[arm] = {
            'registered_cells': CELLS_PER_ARM, 'recorded_cells': len(subset),
            'missing_cells': CELLS_PER_ARM - len(subset),
            'same_k_successes': sum(r['same_k_success'] for r in subset),
            'same_k_success_rate': sum(r['same_k_success'] for r in subset) / CELLS_PER_ARM,
            'generation_outcomes': dict(Counter(r.get('generation_outcome') or 'none' for r in subset)),
            'drafts': drafts, 'admitted': sum(r['admitted'] for r in subset),
            'admission_rate_per_cell': sum(r['admitted'] for r in subset) / CELLS_PER_ARM,
            'admission_rate_per_draft': sum(r['admitted'] for r in subset) / drafts if drafts else None,
            'declines': sum(r['declined'] for r in subset),
            'decline_rate': sum(r['declined'] for r in subset) / CELLS_PER_ARM,
            'invalid_rejected': sum(r['invalid_rejected'] for r in subset),
            'invalid_rejected_rate': sum(r['invalid_rejected'] for r in subset) / CELLS_PER_ARM,
            'attempts_total': sum(r['attempts'] for r in subset),
            'attempts_to_pass': [r['attempts_to_pass'] for r in subset if r['same_k_success']],
            'provider_calls': sum(r.get('provider_calls') or 0 for r in subset),
            'model_calls': sum(r.get('model_calls') or 0 for r in subset),
            'observed_input_tokens': sum(u['input_tokens'] for u in usage) if usage and all(type(u.get('input_tokens')) is int for u in usage) else None,
            'observed_output_tokens': sum(u['output_tokens'] for u in usage) if usage and all(type(u.get('output_tokens')) is int for u in usage) else None,
            'missing_usage_cells': sum(
                arm in 'BC' and (
                    not r.get('usage')
                    or any(not isinstance(u, dict)
                          or not isinstance(u.get('input_tokens'), int)
                          or not isinstance(u.get('output_tokens'), int)
                          for u in r['usage']))
                for r in subset),
            'observed_cost_usd': sum(costs) if costs else None,
            'missing_cost_cells': sum(arm in 'BC' and r.get('cost_usd') is None for r in subset),
            'elapsed_seconds_total': round(sum(timings), 3),
            'elapsed_seconds_median': statistics.median(timings) if timings else None,
            'provider_latency_ms_median': statistics.median(latencies) if latencies else None,
            'errors': sum(r.get('error') is not None for r in subset),
            'violations': [v for r in subset for v in r['violations']],
        }
    index = {k: r for k, r in zip(keys, rows)}
    cases = {}
    robust_vs_a = []
    robust_vs_b = []
    for case in REGISTERED_CASES:
        cases[case] = {arm: [index.get((case, p, arm), {}).get('same_k_success') for p in (0, 1)]
                       for arm in ARMS}
        c_wins = all(index.get((case, p, 'C'), {}).get('same_k_success') is True for p in (0, 1))
        if c_wins and all(index.get((case, p, 'A'), {}).get('same_k_success') is False
                          for p in (0, 1)):
            robust_vs_a.append(case)
        if c_wins and all(index.get((case, p, 'B'), {}).get('same_k_success') is False
                          for p in (0, 1)):
            robust_vs_b.append(case)
    complete = set(keys) == REGISTERED_CELLS
    total_errors = sum(v['errors'] for v in totals.values())
    protocol_violations = sum(len(v['violations']) for v in totals.values())
    gate = (complete
            and total_errors == 0
            and protocol_violations == 0
            and totals['C']['same_k_successes'] > totals['A']['same_k_successes']
            and totals['C']['same_k_successes'] > totals['B']['same_k_successes']
            and len(robust_vs_a) >= 2
            and len(robust_vs_b) >= 2
            and not any(v['violations'] for v in totals.values()))
    return {'protocol': 'E2 prospective holdout preregistration',
            'total_errors': total_errors, 'protocol_violations': protocol_violations,
            'complete': complete, 'arms': totals, 'by_case': cases,
            'robust_c_over_a_cases': robust_vs_a,
            'robust_c_over_b_cases': robust_vs_b,
            'efficacy_gate_passed': gate,
            'elapsed_comparison_valid': False}


if __name__ == '__main__':
    print(json.dumps(summarize(json.loads(Path(sys.argv[1]).read_text())), indent=2))
