"""Offline credential confinement. Only a public synthetic canary is ever used."""
import os
import pathlib
import subprocess
import socket
import threading
import array
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

    def test_only_requester_gets_socket_path_and_no_controller_environment(self):
        local = runner.LocalRun(types.SimpleNamespace(run=self.root, credential_socket='/private-synthetic-channel'),
                                {'credential_wrapper': {'path': str(WRAPPER)}})
        with patch.object(runner, 'verify_binaries'), patch.object(runner.subprocess, 'Popen', return_value=Mock()) as popen:
            for requester in (False, True):
                local.start(['/not/executed', 'config'], self.root/str(requester), requester=requester)
                kwargs = popen.call_args.kwargs
                self.assertTrue(kwargs['close_fds'])
                self.assertNotIn('pass_fds', kwargs)
                self.assertEqual(set(kwargs['env']), {'PATH', 'HOME', 'TMPDIR'})
                self.assertEqual(kwargs['stdin'], subprocess.DEVNULL)
                self.assertEqual(str(WRAPPER) in popen.call_args.args[0], requester)

    def test_no_inherited_secret_environment_fallback(self):
        local = runner.LocalRun(types.SimpleNamespace(run=self.root), {})
        with patch.object(runner, 'verify_binaries'), patch.object(runner.subprocess, 'Popen', return_value=Mock()) as popen:
            local.start(['/not/executed', 'config'], self.root/'requester', requester=True)
            self.assertIsNotNone(popen.call_args.kwargs['env'])
            self.assertTrue(popen.call_args.kwargs['close_fds'])

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_sealed_canary_only_in_child_environment_never_output(self):
        self.inject(sealed=True, valid=True)

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_unsealed_memory_rejected_without_child(self):
        self.inject(sealed=False, valid=True)

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_invalid_credential_rejected_without_child(self):
        self.inject(sealed=True, valid=False)

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_channel_preflight_failure_never_requests_or_reads_credential(self):
        result = self.channel(preflight_ok=False)
        self.assertNotIn(b'READY_FOR_REQUESTER_CREDENTIAL', result.stdout)
        self.assertNotEqual(result.returncode, 0)

    @unittest.skipUnless(sys.platform == 'linux', 'sealed memfd requires Linux')
    def test_channel_keeps_bytes_and_fd_out_of_controller(self):
        result = self.channel(preflight_ok=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn(b'READY_FOR_REQUESTER_CREDENTIAL', result.stdout)
        self.assertTrue((self.root/'delivered').exists())

    def channel(self, preflight_ok):
        driver = self.root/'fake-controller.py'
        child = self.root/'fake-requester.py'
        child.write_text('import os,pathlib\n'
                         'assert os.environ["DEEPSEEK_API_KEY"] == '+repr(CANARY.decode())+'\n'
                         'pathlib.Path('+repr(str(self.root/'delivered'))+').write_text("PASS")\n')
        driver.write_text('import os,pathlib,subprocess,sys\n'
            'assert "DEEPSEEK_API_KEY" not in os.environ\n'
            'for fd in pathlib.Path("/proc/self/fd").iterdir():\n'
            ' try: assert "memfd:requester-credential" not in os.readlink(fd)\n'
            ' except FileNotFoundError: pass\n'
            'if sys.argv[1] == "preflight": sys.exit('+str(0 if preflight_ok else 1)+')\n'
            'address=sys.argv[sys.argv.index("--credential-socket")+1]\n'
            'subprocess.run([sys.executable,'+repr(str(WRAPPER))+',address,sys.executable,'+repr(str(child))+'],check=True)\n')
        argv = [sys.executable, str(WRAPPER.with_name('requester-credential-channel.py')), '--driver', str(driver)]
        for name in ('ato','api','plan','binaries','preflight-run','run','journal','channel-dir'):
            argv += ['--'+name,str(self.root/name)]
        env = dict(PATH='/usr/bin:/bin', HOME=str(self.root), TMPDIR=str(self.root))
        result = subprocess.run(argv,input=CANARY,env=env,capture_output=True,timeout=10)
        self.assertNotIn(CANARY,result.stdout+result.stderr)
        self.assertFalse((self.root/'channel-dir').exists())
        return result

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
            address = str(self.root/'channel')
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as broker:
                broker.bind(address); broker.listen(1)
                def transfer():
                    with broker.accept()[0] as peer:
                        peer.sendmsg([b'K'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))])
                sender = threading.Thread(target=transfer, daemon=True); sender.start()
                result = subprocess.run([sys.executable, str(WRAPPER), address, sys.executable, str(child)],
                                        close_fds=True, env=env, capture_output=True, timeout=10)
                sender.join(timeout=5)

            self.assertNotIn(CANARY, result.stdout+result.stderr)
            self.assertNotIn(b'unrelated-canary', result.stdout+result.stderr)
            self.assertEqual(marker.exists(), sealed and valid)
            self.assertEqual(result.returncode == 0, sealed and valid)
        finally:
            os.close(fd)


if __name__ == '__main__':
    unittest.main()
