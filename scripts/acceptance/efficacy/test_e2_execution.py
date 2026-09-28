"""Offline orchestration tests. All Coordinator/Runtime/provider entrypoints mocked."""
from copy import deepcopy
from dataclasses import dataclass
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from efficacy import e2_execution as execution, e2_oracle as oracle
from efficacy.e2_protocol import ROOT
from e2_test_support import PLAN_DATA, record, oracle_record

spec = importlib.util.spec_from_file_location('e2_primary_test', ROOT / 'scripts/acceptance/formation-generation-efficacy-e2.py')
primary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(primary)


@dataclass
class FakeRun:
    case: str
    root: Path
    sid: str
    owner: str
    token: Path
    fixture_sha256: str = ''
    runtime: object = None
    requester: object = None


class Execution(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=ROOT / '.tmp')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_oracle_failures_abort_before_key_read_and_primary_start(self):
        for error in ('oracle incomplete', 'oracle expectation mismatch', 'oracle missing'):
            with patch.object(sys, 'argv', ['e2', '--run']), \
                 patch.object(primary, 'preflight', return_value={}), \
                 patch.object(primary, 'validate_oracle_result', side_effect=RuntimeError(error)), \
                 patch.object(primary.sys, 'stdin') as stdin, \
                 patch.object(primary, 'run_primary') as run:
                with self.assertRaisesRegex(RuntimeError, error):
                    primary.main()
                stdin.readline.assert_not_called()
                run.assert_not_called()

    def test_code_and_api_drift_abort_before_oracle_key_and_reservation(self):
        for error in ('registered code hash drift', 'API pin drift'):
            with patch.object(sys, 'argv', ['e2', '--run']), \
                 patch.object(primary, 'preflight', side_effect=RuntimeError(error)), \
                 patch.object(primary, 'validate_oracle_result') as validate, \
                 patch.object(primary.sys, 'stdin') as stdin, \
                 patch.object(primary, 'run_primary') as run:
                with self.assertRaisesRegex(RuntimeError, error):
                    primary.main()
                validate.assert_not_called()
                stdin.readline.assert_not_called()
                run.assert_not_called()

    def test_setup_exception_persists_reserved_cell(self):
        context = SimpleNamespace(h=Mock(), d=Mock(), Run=FakeRun,
                                  v1=SimpleNamespace(RUN_ID='unit'), cleanup=Mock(), observe=Mock(return_value={}))
        with patch.object(execution, 'preflight', side_effect=RuntimeError('setup failure')):
            row = execution.execute_cell(context, self.root, 'D01p0C', 'D01', 0, 'C',
                                         'jev_v3', 'v3', PLAN_DATA, {}, key='unit-test-placeholder')
        self.assertTrue((self.root / 'D01p0C.reserved').exists())
        self.assertIn('harness_error', row['violations'])
        self.assertEqual(json.loads((self.root / 'D01p0C/record.json').read_text()), row)
        context.h.runtime.assert_not_called()
        with self.assertRaises(FileExistsError):
            execution.execute_cell(context, self.root, 'D01p0C', 'D01', 0, 'C',
                                   'jev_v3', 'v3', PLAN_DATA, {})

    def test_broken_error_observation_still_persists(self):
        context = SimpleNamespace(h=Mock(), d=Mock(), Run=FakeRun,
                                  v1=SimpleNamespace(RUN_ID='unit'), cleanup=Mock(),
                                  observe=Mock(side_effect=ValueError('broken telemetry')))
        with patch.object(execution, 'preflight', side_effect=RuntimeError('setup failure')):
            row = execution.execute_cell(context, self.root, 'D01p0C', 'D01', 0, 'C',
                                         'jev_v3', 'v3', PLAN_DATA, {})
        self.assertIn('telemetry_read_error', row['violations'])
        self.assertTrue((self.root / 'D01p0C/record.json').is_file())

    def test_primary_stops_after_saving_first_error(self):
        output = self.root / 'primary'
        context = SimpleNamespace(h=Mock())
        row = record('A', 'C01', 0, False)
        row.update(error='provider error', violations=['provider_error'])
        with patch.object(execution, 'OUTPUT', output), \
             patch.object(execution, 'load_execution', return_value=context), \
             patch.object(execution, 'execute_cell', return_value=row) as cell:
            with self.assertRaisesRegex(RuntimeError, 'protocol deviation'):
                execution.run_primary(PLAN_DATA, {}, {'results': []}, 'unit-test-placeholder')
            self.assertEqual(cell.call_count, 1)
        self.assertEqual(len(json.loads((output / 'results.json').read_text())), 1)
        self.assertFalse(json.loads((output / 'run-status.json').read_text())['complete'])
        context.h.stop_coordinator.assert_called_once()

    def oracle_row(self, context, output, key, case, permutation, arm, provider, mode, plan, environment):
        self.assertEqual(permutation, 0)
        self.assertEqual(arm, 'oracle')
        self.assertIn(provider, ('fixed:k4', 'fixed:v9'))
        cell = next(c for c in plan['oracle']['cells'] if c['case'] == case and c['provider'] == provider)
        r = oracle_record(cell)
        r['selected_entrypoint'] = r['evidence']['selected_entrypoint'] = cell['draft_entrypoint_id']
        return r

    def test_oracle_executes_only_exact_registered_fixed_cells_without_key(self):
        # This is a mocked loop, not an oracle execution or an E2 Runtime cell.
        output = self.root / 'oracle'
        with patch.object(oracle, 'ORACLE_OUTPUT', output), \
             patch.object(oracle, 'load_execution', return_value=SimpleNamespace(h=Mock())), \
             patch.object(oracle, 'execute_cell', side_effect=self.oracle_row) as cell:
            result = oracle.run_oracle(PLAN_DATA, {})
            self.assertEqual(cell.call_count, 24)
            self.assertTrue(result['complete'])
            self.assertTrue(result['all_expectations_match'])
            self.assertEqual(result['model_calls'], 0)
            with self.assertRaises(FileExistsError):
                oracle.run_oracle(PLAN_DATA, {})

    def test_oracle_stops_and_saves_on_first_mismatch(self):
        output = self.root / 'oracle'
        row = record('oracle', 'C01', 0, False)  # registered positive
        with patch.object(oracle, 'ORACLE_OUTPUT', output), \
             patch.object(oracle, 'load_execution', return_value=SimpleNamespace(h=Mock())), \
             patch.object(oracle, 'execute_cell', return_value=row) as cell:
            with self.assertRaisesRegex(RuntimeError, 'oracle expectation'):
                oracle.run_oracle(PLAN_DATA, {})
            self.assertEqual(cell.call_count, 1)
        result = json.loads((output / 'oracle-result.json').read_text())
        self.assertEqual(len(result['results']), 1)
        self.assertFalse(result['complete'])
        self.assertFalse(result['all_expectations_match'])

    def test_oracle_rejects_model_keys_without_loading_helpers(self):
        with patch.object(execution, 'verify_registration'), \
             patch.object(execution, 'verify_fixtures'), \
             patch.dict(execution.os.environ, {'ATO_GENERATION_JEV_API_KEY': 'test-placeholder'}), \
             patch.object(execution, 'verify_environment') as environment:
            with self.assertRaisesRegex(RuntimeError, 'ambient model keys'):
                execution.preflight(PLAN_DATA)
            environment.assert_not_called()


if __name__ == '__main__':
    unittest.main()
