#!/usr/bin/env python3
"""E2: --plan writes digests only; --run requires a matching 24-cell oracle PASS.

No key is read and no cell is reserved until the committed registration, code,
fixtures, API checkout, artifacts and oracle evidence have all been verified.
"""
import argparse
import json
from pathlib import Path
import sys

from efficacy.e2_fixtures import CASES, CORRECT, materialize
from efficacy.e2_protocol import PLAN, ROOT, require, safe_record, sha256, validate_oracle_result
from efficacy.e2_execution import ORACLE_OUTPUT, preflight, run_primary


def plan_fixtures(output):
    notes = ROOT / 'apps/formation-worker/fixtures/runtime-network/notes'
    holdout = ROOT / 'apps/formation-worker/fixtures/runtime-network/e2-holdout'
    output.mkdir(parents=True, exist_ok=False)
    fixtures = {}
    for case in CASES:
        entries, files = materialize(notes, holdout, output / case, case, 0)
        fixtures[case] = {
            'files': files,
            'correct_candidate_indices': CORRECT[case],
            'permutations': [
                entries,
                {k: ('candidate_1.py' if v == 'candidate_0.py' else 'candidate_0.py')
                 for k, v in entries.items()},
            ],
        }
    print(json.dumps(fixtures, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('--plan', type=Path)
    group.add_argument('--run', action='store_true')
    parser.add_argument('--oracle-result', type=Path, default=ORACLE_OUTPUT / 'oracle-result.json')
    args = parser.parse_args()
    if args.plan:
        plan_fixtures(args.plan)
        return
    plan = json.loads(PLAN.read_text())
    expected = [f'{case}p{p}{arm}' for case in CASES for p in (0, 1)
                for arm in ('ABC' if p == 0 else 'CBA')]
    require(plan['cell_keys'] == expected, 'preregistered cell order changed')
    environment = preflight(plan)
    oracle = validate_oracle_result(args.oracle_result, plan, sha256(PLAN), environment)
    key = sys.stdin.readline().strip()
    require(bool(key), 'dedicated generation key missing')
    run_primary(plan, environment, oracle, key)


if __name__ == '__main__':
    main()
