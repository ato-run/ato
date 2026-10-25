import importlib.util
from pathlib import Path
import unittest
ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("native_budget", ROOT / "scripts/acceptance/coverage/native-session-budget.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
PLAN = {"measurement_id": "fixture", "aggregate_ceiling": {
    "searches": 4, "D_rounds": 2, "exchanges": 4, "Runtime_attempts": 2, "inspections": 2,
    "sequential_elapsed_seconds": 30}}
def view(sid, rounds, state="stopped", reserved=0):
    return {"search_id": sid, "configuration_ref": "sha256:" + sid, "deadline_ms": 123,
            "exchanges_used": rounds, "inspection_exchanges_completed": 0, "search_elapsed_ms": 1000,
            "progress": {"status": state, "rounds_consumed": rounds,
                         "search_budget": {"attempts": {"used": 0, "reserved": reserved}}}}
class BudgetAuditTests(unittest.TestCase):
    def test_cancelled_and_unanswered_rounds_block_the_next_case(self):
        report = MODULE.audit(PLAN, [view("completed", 1), view("quota", 1)],
                              requested={"D_rounds": 1})
        self.assertFalse(report["admitted"])
        self.assertEqual(report["totals"]["D_rounds"], 2)
    def test_reconnect_or_older_snapshot_cannot_reduce_consumption(self):
        report = MODULE.audit(PLAN, [view("same", 2), view("same", 1)],
                              requested={"D_rounds": 1})
        self.assertFalse(report["admitted"])
        self.assertEqual(report["totals"]["D_rounds"], 2)
        self.assertEqual(report["totals"]["searches"], 1)
    def test_binding_change_and_incomplete_evidence_fail_closed(self):
        newer = view("same", 1); newer["deadline_ms"] = 999
        with self.assertRaisesRegex(ValueError, "binding_changed"):
            MODULE.audit(PLAN, [view("same", 1), newer])
        incomplete = view("same", 1); del incomplete["progress"]["rounds_consumed"]
        with self.assertRaisesRegex(ValueError, "incomplete_budget_evidence"):
            MODULE.audit(PLAN, [incomplete])
    def test_reserved_attempts_and_uncreated_identifiers_are_not_free(self):
        report = MODULE.audit(PLAN, [view("same", 1, reserved=2)], unregistered_identifiers=4,
                              requested={"Runtime_attempts": 1})
        self.assertFalse(report["admitted"])
        self.assertEqual(report["totals"]["Runtime_attempts"], 2)
        self.assertEqual(set(report["exceeded"]), {"searches", "Runtime_attempts"})
    def test_partial_attempt_and_deadline_evidence_cannot_look_like_zero(self):
        for field in ("used", "reserved"):
            incomplete = view("same", 1, reserved=2)
            del incomplete["progress"]["search_budget"]["attempts"][field]
            with self.assertRaisesRegex(ValueError, "incomplete_budget_evidence"):
                MODULE.audit(PLAN, [incomplete])
        incomplete = view("same", 1); del incomplete["deadline_ms"]
        with self.assertRaisesRegex(ValueError, "incomplete_budget_evidence"):
            MODULE.audit(PLAN, [incomplete])
    def test_negative_or_unknown_requested_scope_cannot_bypass_ceiling(self):
        for requested in ({"D_rounds": -1}, {"unknown": 1}, {"D_rounds": True}):
            with self.assertRaisesRegex(ValueError, "budget_arguments_invalid"):
                MODULE.audit(PLAN, [view("same", 1)], requested=requested)
if __name__ == "__main__":
    unittest.main()
