import base64
import importlib.util
from pathlib import Path
import tempfile
import unittest
from urllib.parse import quote

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "agent_leak_scan", ROOT / "scripts/acceptance/coverage/agent-output-leak-scan.py")
SCAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SCAN)


class AgentOutputScanTests(unittest.TestCase):
    def test_private_value_in_tool_output_or_error_is_detected_without_echo(self):
        value = "private-fixture-canary@acceptance.invalid"
        (ROOT / ".tmp").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=ROOT / ".tmp") as directory:
            path = Path(directory) / "native-events.jsonl"
            for encoded in (value, quote(value, safe=""),
                            base64.b64encode(value.encode()).decode()):
                path.write_text('{"tool_output":{"error":"' + encoded + '"}}\n')
                report = SCAN.scan([path], [value])
                self.assertTrue(report["leak_detected"])
                self.assertNotIn(value, str(report))
                self.assertNotIn(directory, str(report))

    def test_safe_metadata_passes_and_a_replaced_transcript_is_refused(self):
        with tempfile.TemporaryDirectory(dir=ROOT / ".tmp") as directory:
            path = Path(directory) / "safe.jsonl"
            path.write_text('{"variable_ref":"variable_fixture","secret":true}\n')
            self.assertFalse(SCAN.scan([path], ["private-fixture-only-value"])["leak_detected"])
            link = Path(directory) / "replaced.jsonl"
            link.symlink_to(path)
            with self.assertRaisesRegex(ValueError, "regular_transcript_required"):
                SCAN.scan([link], ["private-fixture-only-value"])


if __name__ == "__main__":
    unittest.main()
