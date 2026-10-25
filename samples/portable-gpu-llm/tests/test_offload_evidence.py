"""Replay trial evidence and pinned binary output; positive placement inputs are synthetic."""
import hashlib
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).parents[1]))
from offload_evidence import OffloadEvidence, LINE_LIMIT

FIXTURES = Path(__file__).resolve().parents[3] / 'tests' / 'fixtures' / 'portable-gpu-llm' / 'b11429'


def replay(name, chunk_size=4096):
    evidence = OffloadEvidence()
    data = (FIXTURES / name).read_bytes()
    for offset in range(0, len(data), chunk_size):
        evidence.feed('stdout', data[offset:offset + chunk_size])
    return evidence


def records(name='full.callback.jsonl'):
    return [json.loads(line) for line in (FIXTURES / name).read_bytes().splitlines()]


def feed_records(items):
    evidence = OffloadEvidence()
    for item in items:
        evidence.feed('stdout', json.dumps(item).encode() + b'\n')
    return evidence


class OffloadEvidenceTests(unittest.TestCase):
    def test_fixture_digests_and_origin_are_recorded(self):
        provenance = json.loads((FIXTURES / 'provenance.json').read_text())
        for name, digest in provenance['fixtures'].items():
            self.assertEqual(hashlib.sha256((FIXTURES / name).read_bytes()).hexdigest(), digest, name)
        self.assertIn('synthetic', provenance['callback_inputs'])

    def test_real_trial_http_200_log_is_missing_placement_evidence(self):
        diagnostic = json.loads((FIXTURES / 'trial2.diagnostics.json').read_text())
        self.assertEqual(diagnostic['engine_http_status'], 200)
        self.assertEqual(diagnostic['log_bytes_seen']['stderr'], 1164)
        self.assertEqual((FIXTURES / 'trial2.stderr.txt').stat().st_size, 1164)
        self.assertTrue(diagnostic['log_capture_complete'])
        self.assertEqual(diagnostic['log_errors'], [])
        evidence = replay('trial2.stderr.txt').snapshot()
        self.assertEqual(evidence['state'], 'missing')
        self.assertEqual(evidence['observed_layer_assignments'], 0)
        self.assertIsNone(evidence['gpu_placed_layers'])

    def test_fixed_logger_full_and_partial_placement_forms(self):
        full = replay('full.callback.jsonl').snapshot()
        partial = replay('partial.callback.jsonl').snapshot()
        self.assertEqual((full['state'], full['gpu_placed_layers'], full['target_total_layers']), ('full', 29, 29))
        self.assertEqual((partial['state'], partial['gpu_placed_layers'], partial['target_total_layers']), ('insufficient', 28, 29))
        self.assertEqual(partial['placements'][-1], {'layer': 28, 'device': 'CPU'})

    def test_summary_alone_and_plain_output_never_prove_full_placement(self):
        self.assertEqual(feed_records([records()[0], records()[-1]]).snapshot()['state'], 'missing')
        for name in ['suppressed-v3.callback.txt', 'summary-v4.callback.txt', 'placement-v5.callback.txt']:
            self.assertEqual(replay(name).snapshot()['state'], 'missing')

    def test_actual_cpu_model_http_200_is_insufficient_despite_load_success(self):
        audit = json.loads((FIXTURES / 'cpu-negative.audit.json').read_text())
        self.assertEqual(audit['http_status'], 200)
        self.assertTrue(audit['real_model_load'])
        self.assertFalse(audit['real_gpu_execution'])
        self.assertTrue(audit['container_deleted'])
        evidence = replay('cpu-negative.jsonl', chunk_size=127).snapshot()
        self.assertEqual((evidence['state'], evidence['gpu_placed_layers'], evidence['observed_layer_assignments']), ('insufficient', 0, 29))
        self.assertEqual(evidence['reported_offload_summary'], [0, 29])

    def test_buffer_fallback_prevents_full_assignment_success(self):
        evidence = replay('fallback.callback.jsonl').snapshot()
        self.assertEqual(evidence['observed_gpu_layer_assignments'], 29)
        self.assertIsNone(evidence['gpu_placed_layers'])
        self.assertEqual(evidence['state'], 'invalid')
        self.assertEqual(evidence['invalid_reason'], 'tensor_buffer_fallback')
        self.assertIn('using CPU instead', evidence['buffer_fallback'])

    def test_metadata_and_summary_conflict_fail_closed(self):
        items = records()
        items[0]['msg'] = items[0]['msg'].replace('28', '27')
        self.assertEqual(feed_records(items).snapshot()['invalid_reason'], 'model_layer_count_mismatch')
        items = records()
        items[-1]['msg'] = items[-1]['msg'].replace('29/29', '28/29')
        self.assertEqual(feed_records(items).snapshot()['invalid_reason'], 'placement_summary_mismatch')

    def test_cpu_assignment_overrides_optimistic_summary(self):
        items = records('partial.callback.jsonl')
        items[-1] = records()[-1]
        self.assertEqual(feed_records(items).snapshot()['state'], 'insufficient')

    def test_other_device_missing_or_conflicting_layer_fail_closed(self):
        items = records()
        items[1]['msg'] = items[1]['msg'].replace('CUDA0', 'CUDA1')
        self.assertEqual(feed_records(items).snapshot()['state'], 'insufficient')
        self.assertEqual(feed_records(records()[1:]).snapshot()['state'], 'missing')
        self.assertEqual(feed_records(records()[:1] + records()[2:]).snapshot()['state'], 'missing')
        items = records()
        duplicate = dict(items[1], msg=items[1]['msg'].replace('CUDA0', 'CPU'))
        self.assertEqual(feed_records(items + [duplicate]).snapshot()['invalid_reason'], 'layer_assignment_conflict')

    def test_chunked_streams_and_bounded_pending_buffer(self):
        evidence = replay('full.callback.jsonl', chunk_size=1)
        self.assertEqual(evidence.snapshot()['state'], 'full')
        for _ in range(100):
            evidence.feed('stderr', b'x' * 4096)
        self.assertLessEqual(len(evidence.pending['stderr']), LINE_LIMIT)
        evidence.feed('stderr', b'\n')
        self.assertEqual(evidence.snapshot()['state'], 'full')
        self.assertEqual(evidence.snapshot()['discarded_oversized_records'], 1)
        self.assertEqual(len(evidence.placements), 29)

    def test_non_log_and_wrong_level_records_cannot_supply_placement(self):
        items = records()
        for item in items:
            item['level'] = 'warning'
        self.assertEqual(feed_records(items).snapshot()['state'], 'missing')
        for item in items:
            item['type'] = 'request'
        self.assertEqual(feed_records(items).snapshot()['state'], 'missing')


if __name__ == '__main__':
    unittest.main()
