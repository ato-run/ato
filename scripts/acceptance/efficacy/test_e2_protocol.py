"""Evidence failures are synthetic; these tests never launch a Runtime/provider."""
from copy import deepcopy
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from e2_protocol import (ROOT, ARM_IDENTITY, ORACLE_RESULT_SCHEMA, digest, oracle_expectation, oracle_key,
                         oracle_observed_result, terminal_generated_failure, validate_oracle_cells,
                         safe_record, same_k_success, validate_oracle_result,
                         verify_api_pin, verify_registration, write_json)
from e2_test_support import PLAN_DATA, record, snapshot, terminal_snapshot, oracle_record


class Evidence(unittest.TestCase):
    def rec(self, snap, error=None):
        run = SimpleNamespace(root=ROOT / '.tmp/no-such-e2-report',
                              fixture_sha256=digest(PLAN_DATA['fixtures']['D01']['files']))
        return safe_record(run, snap, 'D01', 0, 'C', PLAN_DATA, error)

    def test_success_requires_generated_route(self):
        self.assertTrue(same_k_success(snapshot()))
        self.assertFalse(self.rec(snapshot())['violations'])

    def test_parent_only_pass_is_false(self):
        s = snapshot()
        s['result']['attempts'] = s['result']['attempts'][:1]
        self.assertFalse(same_k_success(s))

    def test_wrong_admitted_ref_is_false(self):
        s = snapshot()
        s['generation_rows'][0]['derivation_ref'] = 'wrong'
        self.assertFalse(same_k_success(s))

    def test_no_generation_row_is_false(self):
        s = snapshot()
        s['generation_rows'] = []
        self.assertFalse(same_k_success(s))

    def test_wrong_receipt_k_or_d_is_false(self):
        for field in ('contract_ref', 'derivation_ref'):
            s = snapshot()
            s['result']['attempts'][-1]['formation_attempt']['receipt'][field] = 'wrong'
            self.assertFalse(same_k_success(s))

    def test_receipt_from_another_attempt_or_request_is_false(self):
        for field in ('attempt_id', 'request_id'):
            s = snapshot()
            s['result']['attempts'][-1]['formation_attempt']['receipt']['execution'][field] = 'old'
            self.assertFalse(same_k_success(s))
            self.assertIn('receipt_not_fresh', self.rec(s)['violations'])

    def test_requester_rejection_is_not_success(self):
        s = snapshot()
        s['requester_exit'] = 1
        self.assertFalse(same_k_success(s))

    def test_multiple_routes_cannot_infer_generated_acceptance(self):
        s = snapshot()
        s['result']['verified_routes'].append(deepcopy(s['result']['verified_routes'][0]))
        self.assertFalse(same_k_success(s))

    def test_error_including_empty_string_is_violation(self):
        for error in ('setup failed', ''):
            r = self.rec(snapshot(), error)
            self.assertIn('harness_error', r['violations'])
            self.assertFalse(r['same_k_success'])

    def test_arm_invariants_reject_every_missing_or_wrong_field(self):
        for arm in 'ABC':
            for field in ('provider_call_count', 'models', 'prompt_versions', 'provider_request'):
                with self.subTest(arm=arm, field=field):
                    s = snapshot(arm)
                    s.pop(field)
                    run = SimpleNamespace(root=ROOT / '.tmp/no-report', fixture_sha256=digest(PLAN_DATA['fixtures']['D01']['files']))
                    self.assertTrue(safe_record(run, s, 'D01', 0, arm, PLAN_DATA)['violations'])

    def test_provider_outage_and_timeout_are_protocol_violations(self):
        for outcome in ('provider_error', 'timeout'):
            s = snapshot()
            s['generation_rows'][0]['outcome'] = outcome
            self.assertIn('provider_' + outcome, self.rec(s)['violations'])

    def test_missing_usage_is_not_zero_filled(self):
        s = snapshot()
        s['usage'] = [None]
        r = self.rec(s)
        self.assertEqual(r['usage'], [None])
        self.assertIn('missing_usage', r['violations'])

    def test_invalid_decline_and_k_failure_can_continue(self):
        for outcome in ('invalid', 'declined', 'admitted'):
            r = record(passed=False, outcome=outcome)
            self.assertFalse(r['same_k_success'])
            self.assertEqual(r['violations'], [], outcome)

    def test_environment_failure_stops(self):
        s = snapshot(passed=False)
        s['result']['attempts'][-1]['failure'] = {'code': 'dependency_unavailable'}
        self.assertIn('unexpected_runtime_failure', self.rec(s)['violations'])

    def test_exact_registered_budget_not_just_cross_arm_equality(self):
        for field in PLAN_DATA['budget'] | {'max_attempts': 2}:
            s = snapshot()
            s['result']['search_state']['frozen']['policy']['budget'][field] += 1
            self.assertIn('registered_budget_drift', self.rec(s)['violations'])
        for field in ('max_generations', 'timeout_ms'):
            s = snapshot()
            s['result']['search_state']['frozen']['policy']['generation'][field] += 1
            self.assertIn('generation_budget_drift', self.rec(s)['violations'])

    def test_null_contract_and_fixture_drift_stop(self):
        s = snapshot()
        s['result']['contract_ref'] = None
        self.assertIn('missing_or_changed_K', self.rec(s)['violations'])
        run = SimpleNamespace(root=ROOT / '.tmp/no-report', fixture_sha256='wrong')
        self.assertIn('fixture_drift', safe_record(run, snapshot(), 'D01', 0, 'C', PLAN_DATA)['violations'])

    def test_missing_receipt_stops_except_nonobservable_primary_failure(self):
        s = snapshot(passed=False)
        s['result']['attempts'][-1]['formation_attempt']['receipt'] = None
        self.assertIn('missing_runtime_receipt', self.rec(s)['violations'])
        s['result']['attempts'][-1]['failure']['code'] = 'candidate_not_observable'
        self.assertEqual(self.rec(s)['violations'], [])

    def test_failed_generation_submission_stops(self):
        s = snapshot()
        s['calls'][0]['submission_accepted'] = False
        self.assertIn('generation_submission_failed', self.rec(s)['violations'])

    def test_missing_timing_and_call_telemetry_stop(self):
        for field in ('calls', 'provider_latency_ms', 'elapsed_seconds'):
            s = snapshot()
            s.pop(field)
            self.assertTrue(self.rec(s)['violations'], field)

    def test_snapshot_error_cannot_be_hidden_by_absent_argument(self):
        s = snapshot()
        s['error'] = 'observation failed'
        self.assertFalse(self.rec(s)['same_k_success'])
        self.assertIn('harness_error', self.rec(s)['violations'])


class OraclePreflight(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=ROOT / '.tmp')
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'oracle-result.json'
        self.environment = {'api_sha': PLAN_DATA['api_main'], 'artifacts': {'requester': 'r', 'runtime': 't', 'wasm': 'w', 'coordinator_tree': 'c'}}
        self.output = {'schema': ORACLE_RESULT_SCHEMA, 'plan_sha256': 'plan', 'environment': self.environment, 'model_calls': 0,
                       'complete': True, 'all_expectations_match': True, 'results': []}
        for cell in PLAN_DATA['oracle']['cells']:
            r = oracle_record(cell)
            r['evidence']['selected_entrypoint'] = r['selected_entrypoint'] = cell['draft_entrypoint_id']
            self.output['results'].append({'cell_key': oracle_key(cell), 'record': r,
                                          'expected_result': cell['expected_result'],
                                          'observed_result': oracle_observed_result(r),
                                          'expectation_matches': True})

    def validate(self):
        write_json(self.path, self.output)
        return validate_oracle_result(self.path, PLAN_DATA, 'plan', self.environment)

    def test_exact_complete_oracle_is_accepted(self):
        self.assertTrue(self.validate()['complete'])

    def test_missing_oracle_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, 'missing'):
            validate_oracle_result(self.path, PLAN_DATA, 'plan', self.environment)

    def test_incomplete_oracle_rejected_even_with_true_summary(self):
        self.output['results'].pop()
        with self.assertRaisesRegex(RuntimeError, 'cell set'):
            self.validate()

    def test_duplicate_or_substituted_oracle_cell_rejected(self):
        for key in (self.output['results'][0]['cell_key'], 'X99p0:fixed:k4'):
            self.output['results'][-1]['cell_key'] = key
            with self.assertRaisesRegex(RuntimeError, 'cell set'):
                self.validate()

    def test_wrong_expectation_rejected(self):
        self.output['results'][0]['expected_result'] = 'verified_k_fail'
        with self.assertRaisesRegex(RuntimeError, 'expectation'):
            self.validate()

    def test_forged_success_flag_cannot_hide_wrong_receipt(self):
        row = self.output['results'][0]
        row['record']['evidence']['result']['attempts'][-1]['formation_attempt']['receipt']['contract_ref'] = 'wrong'
        with self.assertRaisesRegex(RuntimeError, 'evidence'):
            self.validate()

    def test_registration_pin_and_model_call_drift_rejected(self):
        original = deepcopy(self.output)
        for field, value in [('plan_sha256', 'other'), ('environment', {}), ('model_calls', 1),
                             ('complete', False), ('all_expectations_match', False)]:
            self.output = deepcopy(original)
            self.output[field] = value
            with self.assertRaises(RuntimeError, msg=field):
                self.validate()

    def test_verified_k_fail_oracle_requires_fresh_receipt(self):
        cell = next(c for c in PLAN_DATA['oracle']['cells'] if c['expected_result'] == 'verified_k_fail')
        r = record('oracle', cell['case'], 0, False)
        r['selected_entrypoint'] = cell['draft_entrypoint_id']
        self.assertTrue(oracle_expectation(r, cell))
        r['evidence']['result']['attempts'][-1]['formation_attempt']['receipt'] = None
        self.assertFalse(oracle_expectation(r, cell))

    def test_forged_observed_result_rejected(self):
        self.output['results'][0]['observed_result'] = 'terminal_not_observable'
        with self.assertRaisesRegex(RuntimeError, 'expectation/evidence'):
            self.validate()

    def test_terminal_proof_cannot_fill_verified_k_fail_cell(self):
        row = next(r for r in self.output['results'] if r['expected_result'] == 'terminal_not_observable')
        row['expected_result'] = row['observed_result'] = 'verified_k_fail'
        with self.assertRaisesRegex(RuntimeError, 'expectation/evidence'):
            self.validate()

    def test_result_schema_must_be_current(self):
        self.output['schema'] = 'ato.formation-efficacy-e2-oracle/1'
        with self.assertRaisesRegex(RuntimeError, 'schema'):
            self.validate()


class OracleEvidenceClasses(unittest.TestCase):
    def cell(self, expected):
        return next(c for c in PLAN_DATA['oracle']['cells'] if c['expected_result'] == expected)

    def test_preregistered_classes_are_closed_and_labels_unchanged(self):
        from collections import Counter
        from e2_fixtures import CORRECT
        counts = Counter(c['expected_result'] for c in validate_oracle_cells(PLAN_DATA))
        self.assertEqual(counts, {'verified_pass': 12, 'verified_k_fail': 5, 'terminal_not_observable': 7})
        for c in PLAN_DATA['oracle']['cells']:
            self.assertNotIn('expected_fully_satisfied', c)
            positive = int(c['mapped_file'].split('_')[1].split('.')[0]) in CORRECT[c['case']]
            self.assertEqual(c['expected_result'] == 'verified_pass', positive)
        for bad in ('protocol_failure', 'unknown', ['verified_k_fail', 'terminal_not_observable']):
            p = deepcopy(PLAN_DATA)
            p['oracle']['cells'][0]['expected_result'] = bad
            with self.assertRaises(RuntimeError):
                validate_oracle_cells(p)

    def test_cli_terminal_proof_matches_without_becoming_primary_success(self):
        cell = self.cell('terminal_not_observable')
        r = oracle_record(cell)
        self.assertEqual(r['violations'], [])
        self.assertIsNotNone(terminal_generated_failure(r['evidence']))
        self.assertEqual(oracle_observed_result(r), 'terminal_not_observable')
        self.assertTrue(oracle_expectation(r, cell))
        self.assertFalse(same_k_success(r['evidence']))

    def test_receipt_classes_match(self):
        for expected in ('verified_pass', 'verified_k_fail'):
            cell = self.cell(expected)
            r = oracle_record(cell)
            self.assertEqual(oracle_observed_result(r), expected)
            self.assertTrue(oracle_expectation(r, cell))

    def test_unfinished_attempt_record_rejected(self):
        s = terminal_snapshot()
        s['result']['attempts'][-1]['attestation']['attempt_record'] = 'started_unfinished'
        self.assertIsNone(terminal_generated_failure(s))

    def test_unknown_is_not_negative_proof(self):
        for field in ('status', 'unknown_attempts'):
            s = terminal_snapshot()
            if field == 'status':
                s['result']['attempts'][-1]['status'] = 'unknown'
            else:
                s['result']['unknown_attempts'] = [{'attempt_id': 'attempt-generated'}]
            self.assertIsNone(terminal_generated_failure(s))

    def test_generated_d_mismatch_or_parent_d_rejected(self):
        for location in ('row', 'attempt', 'formation', 'attestation', 'snapshot'):
            s = terminal_snapshot()
            attempt = s['result']['attempts'][-1]
            target = {'row': s['generation_rows'][0], 'attempt': attempt,
                      'formation': attempt['formation_attempt'], 'attestation': attempt['attestation'],
                      'snapshot': s}[location]
            target['generated_derivation_ref' if location == 'snapshot' else 'derivation_ref'] = 'sha256:parent'
            self.assertIsNone(terminal_generated_failure(s), location)

    def test_missing_or_duplicate_attempt_rejected(self):
        for duplicate in (False, True):
            s = terminal_snapshot()
            if duplicate:
                s['result']['attempts'].append(deepcopy(s['result']['attempts'][-1]))
            else:
                s['result']['attempts'].pop()
            self.assertIsNone(terminal_generated_failure(s))

    def test_request_search_and_formation_attempt_identity_must_agree(self):
        for location, field in [('result', 'satisfy_id'), ('result', 'search_id'),
                                ('formation', 'attempt_id'), ('formation', 'runtime_id'),
                                ('formation', 'contract_ref'), ('attestation', 'contract_ref')]:
            s = terminal_snapshot()
            attempt = s['result']['attempts'][-1]
            target = {'result': s['result'], 'formation': attempt['formation_attempt'],
                      'attestation': attempt['attestation']}[location]
            target[field] = 'wrong'
            self.assertIsNone(terminal_generated_failure(s), (location, field))

    def test_setup_dependency_timeout_and_generic_failures_are_not_proof(self):
        for code in ('formation_failed', 'dependency_unavailable', 'runtime_offline', 'timeout', 'attempt_record_unfinished'):
            s = terminal_snapshot()
            s['result']['attempts'][-1]['failure']['code'] = code
            s['result']['attempts'][-1]['formation_attempt']['failure']['code'] = code
            self.assertIsNone(terminal_generated_failure(s), code)

    def test_receipt_absence_and_message_alone_are_not_proof(self):
        s = terminal_snapshot()
        s['result']['attempts'][-1]['attestation']['execution_started'] = False
        s['result']['attempts'][-1]['failure']['message'] = 'candidate_not_observable finished cleanup succeeded'
        self.assertIsNone(terminal_generated_failure(s))
        s = terminal_snapshot()
        s['result']['attempts'][-1]['failure']['message'] = 'irrelevant text'
        self.assertIsNotNone(terminal_generated_failure(s))

    def test_unsafe_or_missing_cleanup_cannot_prove_terminal_outcome(self):
        for mutation in ('failed', 'missing', 'not_destroyed', 'receipt', 'route'):
            s = terminal_snapshot()
            f = s['result']['attempts'][-1]['formation_attempt']
            if mutation == 'failed':
                f['outcomes']['cleanup']['state'] = 'failed'
            elif mutation == 'missing':
                f['outcomes'].pop('cleanup')
            elif mutation == 'not_destroyed':
                f['realization']['destroyed'] = False
            elif mutation == 'receipt':
                f['receipt'] = {'fully_satisfied': False}
            else:
                s['result']['verified_routes'] = [{}]
            self.assertIsNone(terminal_generated_failure(s), mutation)

    def test_launch_setup_failure_with_same_code_is_not_terminal_proof(self):
        s = terminal_snapshot()
        s['result']['attempts'][-1]['formation_attempt']['realization']['endpoints'] = {}
        self.assertIsNone(terminal_generated_failure(s))

    def test_class_is_allowed_only_for_registered_cell(self):
        terminal = oracle_record(self.cell('terminal_not_observable'))
        for expected in ('verified_pass', 'verified_k_fail'):
            cell = dict(self.cell('terminal_not_observable'), expected_result=expected)
            self.assertFalse(oracle_expectation(terminal, cell))


class Integrity(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=ROOT / '.tmp')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_registered_code_hash_drift_aborts(self):
        p = deepcopy(PLAN_DATA)
        p['code_sha256']['scripts/acceptance/formation-generation-efficacy-e2.py'] = 'wrong'
        with self.assertRaisesRegex(RuntimeError, 'code hash drift'):
            verify_registration(p)

    def test_uncommitted_registered_harness_is_rejected(self):
        import hashlib
        p = deepcopy(PLAN_DATA)
        for name in p['code_sha256']:
            dest = self.root / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes((ROOT / name).read_bytes())
            p['code_sha256'][name] = hashlib.sha256(dest.read_bytes()).hexdigest()
        plan_path = self.root / 'docs/ops/formation-efficacy-e2-plan.json'
        plan_path.parent.mkdir(parents=True, exist_ok=True)
        write_json(plan_path, p)
        with patch('e2_protocol.git', return_value=' M scripts/acceptance/efficacy/e2_protocol.py'):
            with self.assertRaisesRegex(RuntimeError, 'uncommitted'):
                verify_registration(p, self.root, plan_path)

    def test_api_pin_and_uncommitted_source_drift_abort(self):
        def git(*args):
            return subprocess.check_output(['git', '-C', str(self.root), *args], text=True).strip()
        git('init', '-q')
        (self.root / 'src').mkdir()
        source = self.root / 'src/coordinator.ts'
        source.write_text('pinned')
        git('add', 'src')
        git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'fixture')
        pin = git('rev-parse', 'HEAD')
        self.assertEqual(verify_api_pin(self.root, {'api_main': pin}), pin)
        with self.assertRaisesRegex(RuntimeError, 'API pin drift'):
            verify_api_pin(self.root, {'api_main': 'wrong'})
        source.write_text('modified')
        with self.assertRaisesRegex(RuntimeError, 'uncommitted API'):
            verify_api_pin(self.root, {'api_main': pin})


if __name__ == '__main__':
    unittest.main()
