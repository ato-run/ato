"""Evidence failures are synthetic; these tests never launch a Runtime/provider."""
from copy import deepcopy
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from e2_protocol import (ROOT, ARM_IDENTITY, digest, oracle_expectation, oracle_key,
                         safe_record, same_k_success, validate_oracle_result,
                         verify_api_pin, verify_registration, write_json)
from e2_test_support import PLAN_DATA, record, snapshot


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
        self.output = {'plan_sha256': 'plan', 'environment': self.environment, 'model_calls': 0,
                       'complete': True, 'all_expectations_match': True, 'results': []}
        for cell in PLAN_DATA['oracle']['cells']:
            r = record('oracle', cell['case'], 0, cell['expected_fully_satisfied'])
            r['evidence']['selected_entrypoint'] = r['selected_entrypoint'] = cell['draft_entrypoint_id']
            self.output['results'].append({'cell_key': oracle_key(cell), 'record': r,
                                          'expected_fully_satisfied': cell['expected_fully_satisfied'],
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
        self.output['results'][0]['expected_fully_satisfied'] = False
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

    def test_negative_oracle_requires_fresh_receipt(self):
        cell = next(c for c in PLAN_DATA['oracle']['cells'] if not c['expected_fully_satisfied'])
        r = record('oracle', cell['case'], 0, False)
        r['selected_entrypoint'] = cell['draft_entrypoint_id']
        self.assertTrue(oracle_expectation(r, cell))
        r['evidence']['result']['attempts'][-1]['formation_attempt']['receipt'] = None
        self.assertFalse(oracle_expectation(r, cell))


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
