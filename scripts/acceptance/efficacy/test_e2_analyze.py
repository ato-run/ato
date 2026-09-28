import json
import unittest
from pathlib import Path
from e2_test_support import record
from e2_analyze import REGISTERED_CELLS, REGISTERED_CASES, summarize


class Analysis(unittest.TestCase):
    def rows(self):
        rows = []
        for case in REGISTERED_CASES:
            for p in (0, 1):
                for arm in 'ABC':
                    rows.append(record(arm, case, p, passed=False, outcome='declined'))
        return rows

    def test_declines_remain_in_denominator_and_missing_usage_is_not_zero(self):
        rows = self.rows()
        for row in rows:
            if row['arm'] in 'BC':
                row['usage'] = [None]
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
                r.update(record(r['arm'], r['case'], r['permutation']))
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
                r.update(record(r['arm'], r['case'], r['permutation']))  # A ties C: no excess value
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_gate_needs_c_over_b_aggregate(self):
        rows = self.winning_rows()
        for r in rows:
            if r['arm'] == 'B':
                r.update(record(r['arm'], r['case'], r['permutation']))
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
                r.update(record(r['arm'], r['case'], r['permutation']))
        # Only two robust pairs exist; dropping A/B records cannot fabricate more.
        rows = [r for r in rows if not (r['arm'] == 'A' and r['case'] == 'P02')]
        result = summarize(rows)
        self.assertFalse(result['complete'])
        self.assertFalse(result['efficacy_gate_passed'])


    def test_error_row_blocks_even_without_violations(self):
        rows = self.winning_rows()
        rows[0]['error'] = ''  # is-not-None, not truthiness
        result = summarize(rows)
        self.assertEqual(result['total_errors'], 1)
        self.assertFalse(result['efficacy_gate_passed'])

    def test_missing_model_metadata_blocks(self):
        rows = self.winning_rows()
        rows[1].pop('models')
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_wrong_prompt_blocks(self):
        rows = self.winning_rows()
        rows[2]['prompt_versions'] = ['ato.formation-generation-prompt/2']
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_missing_context_blocks(self):
        rows = self.winning_rows()
        rows[0]['context'] = None
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_provider_error_and_timeout_block(self):
        for outcome in ('provider_error', 'timeout'):
            with self.subTest(outcome=outcome):
                rows = self.winning_rows()
                rows[1]['generation_outcome'] = outcome
                self.assertFalse(summarize(rows)['efficacy_gate_passed'])

    def test_parent_only_pass_does_not_count(self):
        rows = self.winning_rows()
        for row in rows:
            if row['same_k_success']:
                row['evidence']['result']['attempts'] = row['evidence']['result']['attempts'][:1]
        result = summarize(rows)
        self.assertEqual(result['arms']['C']['same_k_successes'], 0)
        self.assertFalse(result['efficacy_gate_passed'])

    def test_receipt_derivation_mismatch_does_not_count(self):
        rows = self.winning_rows()
        for row in rows:
            if row['same_k_success']:
                row['evidence']['result']['attempts'][-1]['formation_attempt']['receipt']['derivation_ref'] = 'wrong'
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])


if __name__ == '__main__':
    unittest.main()
