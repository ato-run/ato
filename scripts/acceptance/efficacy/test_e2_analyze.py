import json
import unittest
from pathlib import Path
from e2_analyze import REGISTERED_CELLS, REGISTERED_CASES, summarize


class Analysis(unittest.TestCase):
    def rows(self):
        rows = []
        for case in REGISTERED_CASES:
            for p in (0, 1):
                for arm in 'ABC':
                    rows.append(dict(case=case, permutation=p, arm=arm, same_k_success=False,
                        draft_proposed=False, admitted=False, declined=True, invalid_rejected=False,
                        attempts=1, attempts_to_pass=None, provider_calls=1,
                        model_calls=int(arm in 'BC'),
                        usage=[], cost_usd=None, elapsed_seconds=1, provider_latency_ms=[],
                        error=None, violations=[]))
        return rows

    def test_declines_remain_in_denominator_and_missing_usage_is_not_zero(self):
        rows = self.rows(); rows[2]['usage'] = [None]
        result = summarize(rows)
        self.assertFalse(result['efficacy_gate_passed'])
        self.assertEqual(result['arms']['C']['decline_rate'], 1)
        self.assertEqual(result['arms']['C']['missing_usage_cells'], 24)
        self.assertEqual(result['arms']['B']['missing_usage_cells'], 24)
        self.assertEqual(result['arms']['A']['missing_usage_cells'], 0)

    def winning_rows(self):
        rows = self.rows()
        for r in rows:
            if r['arm'] == 'C' and r['case'] in ('D01', 'D02', 'P02', 'L01'):
                r['same_k_success'] = True
        return rows

    def test_gate_requires_all_conditions(self):
        result = summarize(self.winning_rows())
        self.assertTrue(result['efficacy_gate_passed'])
        self.assertEqual(sorted(result['robust_c_over_a_cases']), ['D01', 'D02', 'L01', 'P02'])
        self.assertEqual(sorted(result['robust_c_over_b_cases']), ['D01', 'D02', 'L01', 'P02'])

    def test_gate_needs_c_over_a_aggregate(self):
        rows = self.winning_rows()
        for r in rows:
            if r['arm'] == 'A':
                r['same_k_success'] = True  # A ties C: no excess value
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_gate_needs_c_over_b_aggregate(self):
        rows = self.winning_rows()
        for r in rows:
            if r['arm'] == 'B':
                r['same_k_success'] = True
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_robust_case_requires_both_permutations(self):
        rows = self.winning_rows()
        next(r for r in rows if r['arm'] == 'C' and r['case'] == 'L01' and r['permutation'] == 1
             )['same_k_success'] = False
        result = summarize(rows)
        self.assertNotIn('L01', result['robust_c_over_a_cases'])
        self.assertNotIn('L01', result['robust_c_over_b_cases'])
        self.assertTrue(result['efficacy_gate_passed'])  # three cases remain
        next(r for r in rows if r['arm'] == 'C' and r['case'] == 'P02' and r['permutation'] == 0
             )['same_k_success'] = False
        self.assertTrue(summarize(rows)['efficacy_gate_passed'])  # two remain, threshold met
        next(r for r in rows if r['arm'] == 'C' and r['case'] == 'D01' and r['permutation'] == 1
             )['same_k_success'] = False
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])  # only one remains

    def test_comparator_failure_must_be_recorded_not_missing(self):
        rows = self.winning_rows()
        # An unrecorded A cell can never satisfy the both-permutation condition.
        rows = [r for r in rows if not (r['arm'] == 'A' and r['case'] == 'D01'
                                        and r['permutation'] == 0)]
        result = summarize(rows)
        self.assertFalse(result['complete'])
        self.assertNotIn('D01', result['robust_c_over_a_cases'])
        self.assertFalse(result['efficacy_gate_passed'])

    def test_registered_domain_matches_immutable_preregistration(self):
        root = Path(__file__).resolve().parents[3]
        plan = json.loads((root / 'docs/ops/formation-efficacy-e2-plan.json').read_text())
        expected = set()
        for key in plan['cell_keys']:
            case, rest = key.split('p')
            expected.add((case, int(rest[0]), rest[1]))
        self.assertEqual(len(plan['cell_keys']), plan['cells'])
        self.assertEqual(len(plan['cell_keys']), 72)
        self.assertEqual(REGISTERED_CELLS, expected)

    def test_registered_seventy_two_cells_are_complete(self):
        self.assertTrue(summarize(self.rows())['complete'])

    def test_missing_cell_is_incomplete(self):
        self.assertFalse(summarize(self.rows()[:-1])['complete'])

    def test_unregistered_cell_cannot_fill_a_missing_registered_cell(self):
        for field, value in [('case', 'X99'), ('permutation', 2), ('arm', 'D')]:
            with self.subTest(field=field):
                rows = self.winning_rows()
                rows[-1][field] = value
                self.assertEqual(len(rows), 72)
                result = summarize(rows)
                self.assertFalse(result['complete'])
                self.assertFalse(result['efficacy_gate_passed'])

    def test_duplicate_cells_are_rejected(self):
        rows = self.rows()
        with self.assertRaisesRegex(ValueError, 'duplicate cell'):
            summarize(rows + [rows[0]])
        with self.assertRaisesRegex(ValueError, 'duplicate cell'):
            summarize(rows[:-1] + [rows[0]])

    def test_violations_block_the_gate(self):
        rows = self.winning_rows()
        rows[0]['violations'] = ['repeat_generation']
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_missing_rows_never_count_as_success(self):
        rows = self.rows()
        for r in rows:
            if r['arm'] == 'C' and r['case'] in ('D01', 'D02'):
                r['same_k_success'] = True
        # Only two robust pairs exist; dropping A/B records cannot fabricate more.
        rows = [r for r in rows if not (r['arm'] == 'A' and r['case'] == 'P02')]
        result = summarize(rows)
        self.assertFalse(result['complete'])
        self.assertFalse(result['efficacy_gate_passed'])


if __name__ == '__main__':
    unittest.main()
