"""Offline preregistration negatives; never import a credential/HTTP client."""
import copy
import importlib.util
import pathlib
import tempfile
import unittest
from unittest.mock import patch

HERE=pathlib.Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('d3',HERE/'deepseek-live.py')
d3=importlib.util.module_from_spec(spec)
spec.loader.exec_module(d3)

class PreflightTests(unittest.TestCase):
    def test_separate_ceiling_arithmetic(self):
        self.assertEqual(d3.reservations(d3.BUDGET),dict(input_usd_micros=78644,output_usd_micros=2458,per_call_usd_micros=81102,total_usd_micros=486612))

    def test_changed_budget_fails(self):
        for key in d3.BUDGET:
            b=dict(d3.BUDGET);b[key]+=1
            with self.assertRaises(ValueError):d3.reservations(b)

    def test_no_duplicate_json(self):
        with tempfile.TemporaryDirectory(dir=d3.ROOT/'.tmp') as folder:
            p=pathlib.Path(folder)/'bad.json';p.write_text('{"model":"a","model":"b"}')
            with self.assertRaises(ValueError):d3.load(p)

    def test_price_table_uses_peak_not_cache_hit(self):
        rows=[['MODEL','deepseek-flash(1)','deepseek-v4-pro'],['MODEL VERSION','DeepSeek-V4.1-Flash','other'],
              ['1M INPUT TOKENS(CACHE MISS)','OFF-PEAK','$0.15','$0.66'],['PEAK','$0.3','$1.32'],
              ['1M OUTPUT TOKENS','OFF-PEAK','$0.6','$1.98'],['PEAK','$1.2','$3.96']]
        html='<table>'+''.join('<tr>'+''.join('<td>'+v+'</td>' for v in r)+'</tr>' for r in rows)+'</table>'
        with tempfile.TemporaryDirectory(dir=d3.ROOT/'.tmp') as folder:
            p=pathlib.Path(folder)/'price.html';p.write_text(html)
            value=d3.pricing_snapshot(p,'2026-09-28T00:00:00Z')
            self.assertEqual(value['input_price_usd_micros_per_million'],300000)
            self.assertEqual(value['output_price_usd_micros_per_million'],1200000)
            for changed in [html.replace('$0.3','$0.31'),html.replace('$1.2','$1.21'),html.replace('deepseek-flash','unknown')]:
                p.write_text(changed)
                with self.assertRaises(ValueError):d3.pricing_snapshot(p,'2026-09-28T00:00:00Z')

    def test_clean_exact_head_required(self):
        with patch.object(d3,'git',side_effect=['wrong']):
            with self.assertRaises(ValueError):d3.clean_pin(pathlib.Path('.'),'expected')
        with patch.object(d3,'git',side_effect=['expected',' M source']):
            with self.assertRaises(ValueError):d3.clean_pin(pathlib.Path('.'),'expected')
        with patch.object(d3,'git',side_effect=['expected','']):d3.clean_pin(pathlib.Path('.'),'expected')

    def test_fixture_mutation_changes_manifest(self):
        for cell in d3.ORDER:
            files=d3.fixture_files(d3.ROOT,cell)
            self.assertIn('serve.py',files)
            changed=copy.deepcopy(files);changed['serve.py']['sha256']='0'*64
            self.assertNotEqual(d3.sha(d3.json_bytes(files)),d3.sha(d3.json_bytes(changed)))
        self.assertEqual(set(d3.CRITERIA),set(d3.ORDER))

    def test_bad_plan_hash_precedes_git_binary_and_pricing(self):
        with tempfile.TemporaryDirectory(dir=d3.ROOT/'.tmp') as folder:
            p=pathlib.Path(folder)/'plan.json';p.write_text('{}')
            with patch.object(d3,'clean_pin',side_effect=AssertionError('must not reach Git')):
                with self.assertRaisesRegex(ValueError,'plan SHA mismatch'):
                    d3.preflight(p,'0'*64,None,None,None,None,None,None,None,None)

    def test_no_key_reader_or_provider_transport_in_preregistration(self):
        source=(HERE/'deepseek-live.py').read_text()
        for forbidden in ['os.environ','getenv(','.dev.vars','urlopen(', 'requests.post(', 'http.client']:
            self.assertNotIn(forbidden,source)

if __name__=='__main__':unittest.main()
