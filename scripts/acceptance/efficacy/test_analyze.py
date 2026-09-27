import unittest
from analyze import summarize

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
    def test_missing_or_duplicate_cells_cannot_close_gate(self):
        rows=self.rows();self.assertFalse(summarize(rows[:-1])['complete'])
        with self.assertRaises(ValueError):summarize(rows+[rows[0]])

if __name__=='__main__':unittest.main()
