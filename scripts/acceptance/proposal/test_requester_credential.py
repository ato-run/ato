"""Offline credential confinement. Only a public synthetic canary is ever used."""
import os
import pathlib
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from execution import runner

ROOT = pathlib.Path(__file__).resolve().parents[3]
WRAPPER = pathlib.Path(__file__).with_name('requester-credential-exec.py')
CANARY = b'public-offline-synthetic-canary-not-a-secret'


class CredentialConfinement(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=ROOT/'.tmp')
        self.root = pathlib.Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_only_requester_gets_fd_and_no_controller_environment(self):
        local = runner.LocalRun(types.SimpleNamespace(run=self.root, credential_fd=73),
                                {'credential_wrapper': {'path': str(WRAPPER)}})
        with patch.object(runner, 'verify_binaries'), patch.object(runner.subprocess, 'Popen', return_value=Mock()) as popen:
            for requester in (False, True):
                local.start(['/not/executed', 'config'], self.root/str(requester), requester=requester)
                kwargs = popen.call_args.kwargs
                self.assertEqual(kwargs['pass_fds'], (73,) if requester else ())
                self.assertEqual(set(kwargs['env']), {'PATH', 'HOME', 'TMPDIR'})
                self.assertEqual(kwargs['stdin'], subprocess.DEVNULL)
                self.assertEqual(str(WRAPPER) in popen.call_args.args[0], requester)

    def test_no_inherited_secret_environment_fallback(self):
        local = runner.LocalRun(types.SimpleNamespace(run=self.root), {})
        with patch.object(runner, 'verify_binaries'), patch.object(runner.subprocess, 'Popen', return_value=Mock()) as popen:
            local.start(['/not/executed', 'config'], self.root/'requester', requester=True)
            self.assertIsNotNone(popen.call_args.kwargs['env'])
            self.assertEqual(popen.call_args.kwargs['pass_fds'], ())

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_sealed_canary_only_in_child_environment_never_output(self):
        self.inject(sealed=True, valid=True)

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_unsealed_memory_rejected_without_child(self):
        self.inject(sealed=False, valid=True)

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_invalid_credential_rejected_without_child(self):
        self.inject(sealed=True, valid=False)

    def inject(self, sealed, valid):
        import fcntl
        fd = os.memfd_create('synthetic-offline-test', os.MFD_ALLOW_SEALING)
        marker = self.root/'executed'
        child = self.root/'child.py'
        child.write_text('import os,pathlib\n'
                         'assert os.environ["DEEPSEEK_API_KEY"] == '+repr(CANARY.decode())+'\n'
                         'assert "UNRELATED_SECRET" not in os.environ\n'
                         'pathlib.Path('+repr(str(marker))+').write_text("PASS")\n')
        try:
            os.write(fd, CANARY if valid else b'bad\nvalue')
            if sealed:
                fcntl.fcntl(fd, fcntl.F_ADD_SEALS, fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL)
            env = dict(PATH='/usr/bin:/bin', HOME=str(self.root), TMPDIR=str(self.root), UNRELATED_SECRET='unrelated-canary')
            result = subprocess.run([sys.executable, str(WRAPPER), str(fd), sys.executable, str(child)],
                                    pass_fds=(fd,), env=env, capture_output=True, timeout=10)
            self.assertNotIn(CANARY, result.stdout+result.stderr)
            self.assertNotIn(b'unrelated-canary', result.stdout+result.stderr)
            self.assertEqual(marker.exists(), sealed and valid)
            self.assertEqual(result.returncode == 0, sealed and valid)
        finally:
            os.close(fd)


if __name__ == '__main__':
    unittest.main()
