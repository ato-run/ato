"""Offline preflight: no source execution or model calls."""
import tempfile
import unittest
from pathlib import Path
from fixtures import CASES, materialize

ROOT = Path(__file__).resolve().parents[3]
NOTES = ROOT/'apps/formation-worker/fixtures/runtime-network/notes'

class Fixtures(unittest.TestCase):
    def test_permutations_change_only_mapping_and_all_sources_parse(self):
        (ROOT/'.tmp').mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=ROOT/'.tmp') as root:
            for case in CASES:
                a,ha=materialize(NOTES,Path(root)/case/'p0',case,0)
                b,hb=materialize(NOTES,Path(root)/case/'p1',case,1)
                self.assertEqual(ha,hb)
                self.assertEqual(a['q7'],b['m2'])
                self.assertEqual(a['m2'],b['q7'])
                for p in (Path(root)/case/'p0').glob('*.py'):
                    compile(p.read_bytes(),'<fixture>','exec')
                if case=='E06':
                    self.assertGreater((Path(root)/'E06/p0/candidate_0.py').stat().st_size,65536)
                if case=='E07':
                    with self.assertRaises(UnicodeDecodeError):
                        (Path(root)/case/'p0/candidate_0.py').read_bytes().decode('utf-8')

if __name__=='__main__': unittest.main()
