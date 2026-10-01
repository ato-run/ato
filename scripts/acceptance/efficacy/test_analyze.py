import json
import unittest
from pathlib import Path
from analyze import REGISTERED_CELLS, summarize

class Analysis(unittest.TestCase):
    def rows(self):
        rows=[]
        for case in [f'E{i:02}' for i in range(1,11)]:
            for p in (0,1):
                for arm in 'ABC':
                    rows.append(dict(case=case,permutation=p,arm=arm,same_k_success=False,
                        draft_proposed=False,admitted=False,declined=True,invalid_rejected=False,
                        attempts=1,attempts_to_pass=None,provider_calls=1,model_calls=int(arm=='C'),
                        usage=[],cost_usd=None,elapsed_seconds=1,provider_latency_ms=[],error=None,violations=[]))
        return rows
    def test_declines_remain_in_denominator_and_missing_usage_is_not_zero(self):
        rows=self.rows(); rows[2]['usage']=[None]
        result=summarize(rows);self.assertFalse(result['efficacy_gate_passed'])
        self.assertEqual(result['arms']['C']['decline_rate'],1)
        self.assertEqual(result['arms']['C']['missing_usage_cells'],20)
    def test_two_cases_must_win_both_permutations(self):
        rows=self.rows()
        for r in rows:
            if r['arm']=='C' and r['case'] in ('E01','E02'):
                r['same_k_success']=True
        self.assertTrue(summarize(rows)['efficacy_gate_passed'])
        next(r for r in rows if r['arm']=='C' and r['case']=='E02' and r['permutation']==1)['same_k_success']=False
        self.assertFalse(summarize(rows)['efficacy_gate_passed'])
    def test_registered_domain_matches_immutable_preregistration(self):
        root = Path(__file__).resolve().parents[3]
        plan = json.loads((root / 'docs/ops/formation-efficacy-e1-plan.json').read_text())
        expected = {
            (case, permutation, arm)
            for case, fixture in plan['fixtures'].items()
            for permutation in range(len(fixture['permutations']))
            for arm in plan['arms']
        }
        self.assertEqual(len(expected), plan['cells'])
        self.assertEqual(REGISTERED_CELLS, expected)

    def test_registered_sixty_cells_are_complete(self):
        self.assertTrue(summarize(self.rows())['complete'])

    def test_missing_cell_is_incomplete(self):
        self.assertFalse(summarize(self.rows()[:-1])['complete'])

    def test_unregistered_cell_cannot_fill_a_missing_registered_cell(self):
        for field, value in [('case', 'E11'), ('permutation', 2), ('arm', 'D')]:
            with self.subTest(field=field):
                rows = self.rows()
                # Make the other gate conditions true so completeness alone
                # prevents a substituted cell from falsely closing the gate.
                for row in rows:
                    if row['arm'] == 'C' and row['case'] in ('E01', 'E02'):
                        row['same_k_success'] = True
                rows[-1][field] = value
                self.assertEqual(len(rows), 60)
                result = summarize(rows)
                self.assertFalse(result['complete'])
                self.assertFalse(result['efficacy_gate_passed'])

    def test_duplicate_cells_are_rejected(self):
        rows = self.rows()
        with self.assertRaisesRegex(ValueError, 'duplicate cell'):
            summarize(rows + [rows[0]])
        with self.assertRaisesRegex(ValueError, 'duplicate cell'):
            summarize(rows[:-1] + [rows[0]])

if __name__=='__main__':unittest.main()
