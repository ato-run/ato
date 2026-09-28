"""Request evidence preflight negatives. No credential or HTTP access."""
import copy
import importlib.util
import pathlib
import unittest
from unittest.mock import patch

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('d3v2', HERE/'deepseek-v2.py')
v2 = importlib.util.module_from_spec(spec); spec.loader.exec_module(v2)
ROOT = HERE.parents[2]


class EvidencePreflightTests(unittest.TestCase):
    def setUp(self):
        self.old = v2.original(ROOT/'docs/ops/formation-deepseek-d3-plan.json')
        self.plan = copy.deepcopy(self.old)
        self.plan.update(schema=v2.SCHEMA, ato_sha='new-execution-pin',
                         supersedes={'plan_sha256':v2.V1_SHA,'reason':v2.REASON},
                         request_evidence=v2.contract(ROOT))

    def reject(self, mutate):
        mutate(self.plan)
        with patch.object(v2.v1, 'clean_pin'):
            with self.assertRaises(ValueError): v2.check(self.plan, self.old, ROOT)

    def test_N15_capture_implementation_sha_drift(self):
        self.reject(lambda p: p['request_evidence']['implementation_files']['capture'].update(sha256='wrong'))

    def test_N16_request_evidence_schema_drift(self):
        self.reject(lambda p: p['request_evidence']['journal_event']['request'].update(provider_body_bytes='body_text'))

    def test_N17_final_timeout_formula_drift(self):
        self.reject(lambda p: p['request_evidence']['timeout_formula'].update(completion_reserve_ms=0))

    def test_N18_request_journal_helper_mismatch(self):
        self.reject(lambda p: p['request_evidence']['implementation_files']['helper'].update(sha256='wrong'))

    def test_N19_provider_body_serializer_drift(self):
        self.reject(lambda p: p['request_evidence'].update(provider_serialization='reqwest.json'))

    def test_no_fixture_K_model_price_or_policy_changes(self):
        for key in ['cells','transport','budget','prompt','source_policy','stop_policy','pricing_snapshot']:
            with self.subTest(key=key):
                saved = copy.deepcopy(self.plan)
                self.reject(lambda p: p.update({key:None}))
                self.plan = saved

    def test_only_evidence_delta_allowed(self):
        with patch.object(v2.v1, 'clean_pin') as pin:
            v2.check(self.plan, self.old, ROOT)
            pin.assert_called_once_with(ROOT, 'new-execution-pin')


if __name__ == '__main__': unittest.main()
