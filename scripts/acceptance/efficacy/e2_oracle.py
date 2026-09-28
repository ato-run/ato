#!/usr/bin/env python3
"""Explicit zero-model E2 Runtime oracle. Never imported with execution effects."""
import argparse
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from efficacy.e2_protocol import (PLAN, ORACLE_RESULT_SCHEMA, oracle_key, oracle_expectation,
                                  oracle_observed_result, require, sha256,
                                  validate_oracle_cells, write_json)
from efficacy.e2_execution import (ORACLE_OUTPUT, check_peers, execute_cell,
                                   load_execution, preflight)


def run_oracle(plan, environment):
    cells = validate_oracle_cells(plan)
    ORACLE_OUTPUT.mkdir(parents=True, exist_ok=False)
    write_json(ORACLE_OUTPUT / 'registration.json', plan, exclusive=True)
    output = {'schema': ORACLE_RESULT_SCHEMA, 'plan_sha256': sha256(PLAN), 'environment': environment, 'results': [],
              'model_calls': 0, 'complete': False, 'all_expectations_match': False}
    context = None
    try:
        context = load_execution(ORACLE_OUTPUT)
        context.h.start_coordinator('efficacy-e2-oracle')
        for cell in cells:
            key = oracle_key(cell)
            record = execute_cell(context, ORACLE_OUTPUT, key, cell['case'], 0, 'oracle',
                                  cell['provider'], 'v2', plan, environment)
            check_peers(record, [r['record'] for r in output['results']
                                 if r['record']['case'] == cell['case']])
            matched = oracle_expectation(record, cell)
            output['results'].append({'cell_key': key, 'record': record,
                                      'expected_result': cell['expected_result'],
                                      'observed_result': oracle_observed_result(record),
                                      'expectation_matches': matched})
            output['model_calls'] += record['model_calls']
            write_json(ORACLE_OUTPUT / 'oracle-result.json', output)
            require(matched, 'oracle expectation/protocol failure; cell saved')
        output['complete'] = True
        output['all_expectations_match'] = True
    except Exception as exc:
        output['error'] = type(exc).__name__ + ': ' + str(exc)
        raise
    finally:
        try:
            if context is not None:
                context.h.stop_coordinator()
        except Exception as exc:
            output['error'] = 'Coordinator cleanup: ' + type(exc).__name__
            output['complete'] = output['all_expectations_match'] = False
            raise
        finally:
            write_json(ORACLE_OUTPUT / 'oracle-result.json', output)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', required=True)
    parser.parse_args()
    plan = json.loads(PLAN.read_text())
    validate_oracle_cells(plan)
    environment = preflight(plan)
    run_oracle(plan, environment)


if __name__ == '__main__':
    main()
