"""Offline E2 fixture preflight: no source execution or model calls."""
import json
import tempfile
import unittest
from pathlib import Path
from e2_fixtures import CASES, CORRECT, IDS, materialize

ROOT = Path(__file__).resolve().parents[3]
RUNTIME_NETWORK = ROOT/'apps/formation-worker/fixtures/runtime-network'
NOTES = RUNTIME_NETWORK/'notes'
HOLDOUT = RUNTIME_NETWORK/'e2-holdout'


class Fixtures(unittest.TestCase):
    def materialize_all(self):
        (ROOT/'.tmp').mkdir(exist_ok=True)
        tmp = tempfile.TemporaryDirectory(dir=ROOT/'.tmp')
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        trees = {}
        for case in CASES:
            a, ha = materialize(NOTES, HOLDOUT, root/case/'p0', case, 0)
            b, hb = materialize(NOTES, HOLDOUT, root/case/'p1', case, 1)
            trees[case] = (a, ha, b, hb)
        return root, trees

    def test_registered_domain(self):
        self.assertEqual(len(CASES), 12)
        self.assertEqual(sorted(CASES), CASES)
        self.assertEqual(set(CORRECT), set(CASES))
        self.assertEqual(len(IDS), 2)

    def test_permutations_change_only_mapping_and_all_sources_parse(self):
        root, trees = self.materialize_all()
        for case in CASES:
            a, ha, b, hb = trees[case]
            self.assertEqual(ha, hb, case)
            self.assertEqual(a[IDS[0]], b[IDS[1]], case)
            self.assertEqual(a[IDS[1]], b[IDS[0]], case)
            for p in (root/case/'p0').glob('*.py'):
                compile(p.read_bytes(), '<fixture>', 'exec')

    def test_no_e1_byte_reuse(self):
        e1 = json.loads((ROOT/'docs/ops/formation-efficacy-e1-plan.json').read_text())
        e1_hashes = {h for fixture in e1['fixtures'].values() for h in fixture['files'].values()}
        _root, trees = self.materialize_all()
        candidate_files = {'candidate_0.py', 'candidate_1.py', 'bad.py',
                           'notes_app.py', 'site_server.py', 'health_service.py',
                           'private-source.txt'}
        for case in CASES:
            leaked = {n for n, h in trees[case][1].items()
                      if n in candidate_files and h in e1_hashes}
            self.assertEqual(leaked, set(), case)

    def test_delegation_targets_exist_and_wrappers_are_distinct(self):
        root, _trees = self.materialize_all()
        wrappers = set()
        for case in ('D01', 'D02', 'D03'):
            c0 = (root/case/'p0'/'candidate_0.py').read_bytes()
            wrappers.add(c0)
            self.assertIn(b'runpy.run_path(', c0, case)
            self.assertIn(b'run_name="__main__"', c0, case)
            target = c0.split(b'run_path("')[1].split(b'"')[0].decode()
            self.assertTrue((root/case/'p0'/target).exists(), (case, target))
            # The delegation target itself is never a registered entrypoint.
            self.assertNotIn(target, ('candidate_0.py', 'candidate_1.py'))
        self.assertEqual(len(wrappers), 3)

    def test_bounded_prefix_cases_are_over_64kib(self):
        root, _trees = self.materialize_all()
        for case in ('P01', 'P02', 'P03'):
            size = (root/case/'p0'/'candidate_0.py').stat().st_size
            self.assertGreater(size, 65536, case)
        for case in ('P01', 'P03'):
            self.assertGreater((root/case/'p0'/'candidate_1.py').stat().st_size, 65536, case)

    def test_latin1_positives_are_not_utf8(self):
        root, _trees = self.materialize_all()
        for case in ('L01', 'L02'):
            with self.assertRaises(UnicodeDecodeError, msg=case):
                (root/case/'p0'/'candidate_0.py').read_bytes().decode('utf-8')
            head = (root/case/'p0'/'candidate_0.py').read_bytes().split(b'\n')[0]
            self.assertIn(b'coding', head, case)

    def test_control_labels_match_spec(self):
        self.assertEqual(CORRECT['C02'], [0, 1])
        self.assertEqual(CORRECT['C03'], [])
        self.assertEqual(CORRECT['C04'], [0])

    def test_indistinguishable_control_differs_only_in_flag(self):
        root, _trees = self.materialize_all()
        c0 = (root/'C04'/'p0'/'candidate_0.py').read_bytes()
        c1 = (root/'C04'/'p0'/'candidate_1.py').read_bytes()
        self.assertEqual(c0.replace(b'READY = True', b'READY = False'), c1)


if __name__ == '__main__':
    unittest.main()
