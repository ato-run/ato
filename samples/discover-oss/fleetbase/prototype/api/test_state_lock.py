import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from state_lock import acquire_state_lock


class StateLockTests(unittest.TestCase):
    def test_other_process_is_excluded_until_lock_is_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            fd = acquire_state_lock(tmp)
            code = ('import os,sys; from state_lock import acquire_state_lock; '
                    'fd=acquire_state_lock(sys.argv[1]); os.close(fd)')
            env = os.environ.copy()
            env['PYTHONPATH'] = str(Path(__file__).parent)
            try:
                blocked = subprocess.run([sys.executable, '-c', code, tmp], env=env,
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertNotEqual(blocked.returncode, 0)
                self.assertIn(b'BlockingIOError', blocked.stderr)
            finally:
                os.close(fd)
            released = subprocess.run([sys.executable, '-c', code, tmp], env=env,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertEqual(released.returncode, 0, released.stderr)

    def test_symlink_and_missing_directory_are_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / 'state'
            root.mkdir()
            link = Path(tmp) / 'link'
            link.symlink_to(root, target_is_directory=True)
            for path in (link, Path(tmp) / 'missing'):
                with self.assertRaises(OSError):
                    acquire_state_lock(path)


if __name__ == '__main__':
    unittest.main()
