import importlib.util
from pathlib import Path
import tempfile
import unittest
import subprocess
import sys
import os
import signal
import time

SPEC = importlib.util.spec_from_file_location("monitor", Path(__file__).with_name("unknown-workload-monitor.py"))
MONITOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MONITOR)


class WorkloadObservation(unittest.TestCase):
    def setUp(self):
        Path(".tmp").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=".tmp")
        self.root = Path(self.temp.name).resolve()
        self.node = self.root / "pinned-node"
        self.node.write_bytes(b"fixture executable identity")

    def tearDown(self):
        self.temp.cleanup()

    def process(self, pid, parent, ticks, node=False, state="S"):
        folder = self.root / str(pid)
        folder.mkdir()
        fields = [state, str(parent)] + ["0"] * 17 + [str(ticks)]
        (folder / "stat").write_text(f"{pid} (name with spaces) " + " ".join(fields))
        if node:
            (folder / "exe").symlink_to(self.node)
        return folder

    def test_materialized_file_id_needs_no_Source_filename_or_argv(self):
        self.process(10, 1, 100)
        self.process(11, 10, 101)
        candidate = self.process(12, 11, 102, node=True)
        (candidate / "cmdline").write_bytes(b"node\0/app/f_opaque_entrypoint\0")
        proof = MONITOR.observe(10, "100", self.node, self.root)
        self.assertEqual(proof["pid"], 12)
        self.assertTrue(proof["workload_actually_started"])
        self.assertNotIn("argv", proof)

    def test_other_Node_process_is_not_the_Runtime_workload(self):
        self.process(10, 1, 100)
        self.process(12, 1, 102, node=True)
        self.assertIsNone(MONITOR.observe(10, "100", self.node, self.root))

    def test_mount_namespace_path_does_not_change_executable_identity(self):
        self.process(10, 1, 100)
        candidate = self.process(12, 10, 102, node=True)
        guest = self.root / "toolchain/bin/node"
        guest.parent.mkdir(parents=True)
        guest.write_bytes(self.node.read_bytes())
        (candidate / "exe").unlink()
        (candidate / "exe").symlink_to(guest)
        self.assertEqual(MONITOR.observe(10, "100", self.node, self.root)["pid"], 12)

    def test_same_executable_name_with_different_bytes_is_rejected(self):
        self.process(10, 1, 100)
        candidate = self.process(12, 10, 102, node=True)
        other = self.root / "unrelated/bin/node"
        other.parent.mkdir(parents=True)
        other.write_bytes(b"different executable")
        (candidate / "exe").unlink()
        (candidate / "exe").symlink_to(other)
        self.assertIsNone(MONITOR.observe(10, "100", self.node, self.root))

    def test_reused_Runtime_pid_is_rejected(self):
        self.process(10, 1, 200)
        with self.assertRaisesRegex(ValueError, "runtime_identity_changed"):
            MONITOR.observe(10, "100", self.node, self.root)

    def test_terminated_workload_is_not_a_start_observation(self):
        self.process(10, 1, 100)
        self.process(12, 10, 102, node=True, state="Z")
        self.assertIsNone(MONITOR.observe(10, "100", self.node, self.root))

    @unittest.skipUnless(sys.platform == "linux", "real proc observation requires Linux")
    def test_live_Node_child_from_owner_fixture_not_Source_Search(self):
        node = Path("/opt/ato/toolchains/node/22.14.0/bin/node")
        if not node.exists():
            self.skipTest("pinned test Node unavailable")
        code = ("import subprocess,time; "
                f"subprocess.Popen([{str(node)!r},'-e','setInterval(()=>{{}},1000)']); "
                "time.sleep(30)")
        supervisor = subprocess.Popen([sys.executable, "-c", code], start_new_session=True,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            ticks = MONITOR.process_stat(Path(f"/proc/{supervisor.pid}/stat").read_text())["start_ticks"]
            end = time.monotonic() + 3
            proof = None
            while proof is None and time.monotonic() < end:
                proof = MONITOR.observe(supervisor.pid, ticks, node)
                time.sleep(0.01)
            self.assertIsNotNone(proof)
            self.assertNotEqual(proof["pid"], supervisor.pid)
            self.assertEqual(proof["runtime_pid"], supervisor.pid)
        finally:
            os.killpg(supervisor.pid, signal.SIGTERM)
            supervisor.wait(timeout=3)


if __name__ == "__main__":
    unittest.main()
