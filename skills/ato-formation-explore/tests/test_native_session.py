"""Native launch admission boundaries; no provider inference or Source run."""
import importlib.util
import json
from pathlib import Path
import tempfile
import time
import unittest

SKILL = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("native_session", SKILL / "scripts/native-session.py")
NATIVE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NATIVE)


class NativeAdmissionTests(unittest.TestCase):
    def setUp(self):
        self.binding = {"search_id": "search-fixture", "configuration_ref": "sha256:" + "a" * 64,
                        "agent": {"kind": "codex", "version": "0.160.0"}}
        self.view = {"search_id": self.binding["search_id"],
                     "configuration_ref": self.binding["configuration_ref"],
                     "deadline_ms": int(time.time() * 1000) + 60000,
                     "exchange_deadline_ms": int(time.time() * 1000) + 10000,
                     "connected": True, "next_operation": "submit",
                     "progress": {"status": "running", "unresolved_attempts": 0},
                     "exchange": {"response_saved": False}}

    def admit(self):
        return NATIVE.admission(self.view, self.binding, "codex", "0.160.0")

    def test_original_deadline_preserved_and_expired_round_rejected(self):
        self.assertEqual(self.admit(), self.view["deadline_ms"])
        self.view["exchange_deadline_ms"] = int(time.time() * 1000) - 1
        with self.assertRaisesRegex(NATIVE.ISOLATION.Rejected, "deadline_exceeded"):
            self.admit()

    def test_saved_response_disconnection_and_unknown_do_not_start_inference(self):
        for change in ({"connected": False}, {"exchange": {"response_saved": True}},
                       {"progress": {"status": "running", "unresolved_attempts": 1}},
                       {"next_operation": "owner_reconcile"}):
            view = dict(self.view, **change)
            with self.subTest(change=change), self.assertRaises(NATIVE.ISOLATION.Rejected):
                NATIVE.admission(view, self.binding, "codex", "0.160.0")

    def test_input_wait_and_terminal_states_do_not_reopen(self):
        for progress in ({"status": "running", "pause_reason": "needs_input"},
                         {"status": "k_reached_awaiting_assessment"},
                         {"status": "cancelled"}, {"status": "failed"}):
            with self.subTest(progress=progress), self.assertRaises(NATIVE.ISOLATION.Rejected):
                NATIVE.admission(dict(self.view, progress=progress), self.binding, "codex", "0.160.0")

    def test_another_agent_or_search_cannot_attach(self):
        for view, agent, version in ((dict(self.view, search_id="another"), "codex", "0.160.0"),
                                     (self.view, "claude-code", "2.1.288"),
                                     (self.view, "codex", "0.46.0")):
            with self.assertRaisesRegex(NATIVE.ISOLATION.Rejected, "binding_mismatch"):
                NATIVE.admission(view, self.binding, agent, version)

    def test_known_pending_runtime_waits_without_inference(self):
        for attempt_status in ("pending", "claimed"):
            view = dict(self.view, next_operation="owner_reconcile", progress={
                "status": "running", "unresolved_attempts": 1,
                "attempts": [{"status": attempt_status}]})
            self.assertEqual(NATIVE.next_action(view), "wait")
            with self.assertRaises(NATIVE.ISOLATION.Rejected):
                NATIVE.admission(view, self.binding, "codex", "0.160.0")

    def test_unknown_or_unaccounted_attempt_stops_even_with_active_runtime(self):
        for attempts, count in (([{"status": "unknown"}], 1),
                                 ([{"status": "claimed"}], 2),
                                 ([{"status": "claimed"}, {"status": "unknown"}], 2)):
            view = dict(self.view, next_operation="owner_reconcile", progress={
                "status": "running", "unresolved_attempts": count, "attempts": attempts})
            self.assertEqual(NATIVE.next_action(view), "stop")

    def test_saved_response_waits_but_disconnect_or_input_pause_stops(self):
        view = dict(self.view, next_operation="owner_reconcile", exchange={"response_saved": True})
        self.assertEqual(NATIVE.next_action(view), "wait")
        self.assertEqual(NATIVE.next_action(dict(view, connected=False)), "stop")
        self.assertEqual(NATIVE.next_action(dict(view, progress={
            "status": "running", "pause_reason": "needs_input"})), "stop")

    def test_model_catalog_removes_host_tools_without_changing_model_prompt(self):
        temporary = SKILL.parents[1] / ".tmp"
        temporary.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=temporary) as directory:
            path = Path(directory) / "catalog.json"
            model = {"slug": "fixture-model", "base_instructions": "official fixture prompt",
                     "shell_type": "shell_command", "apply_patch_tool_type": "freeform",
                     "experimental_supported_tools": ["image_gen"]}
            path.write_text(json.dumps({"models": [model, {"slug": "other"}]}))
            narrowed = NATIVE.narrowed_catalog(path, "fixture-model")["models"]
            self.assertEqual(len(narrowed), 1)
            self.assertEqual(narrowed[0]["base_instructions"], model["base_instructions"])
            self.assertIsNone(narrowed[0]["apply_patch_tool_type"])
            self.assertEqual(narrowed[0]["shell_type"], "disabled")
            self.assertEqual(narrowed[0]["experimental_supported_tools"], [])


if __name__ == "__main__":
    unittest.main()
