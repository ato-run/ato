#!/usr/bin/env python3
"""One paired live acceptance on the existing isolated Stage 4 Coordinator.

Install beside stage4_acceptance.py and formation-5a-decision.py on the
acceptance host. Requires ATO_DECISION_JEV_API_KEY in the process environment.
No retries: one Jev decision, max_decisions=1, two attempts per search.
Both arms use the same decision policy; baseline selects the core's default
locally (zero external provider calls). Separate owners prevent retained-route
reuse from contaminating the second arm. No production/staging services.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

import stage4_acceptance as h

spec = importlib.util.spec_from_file_location(
    "decision_acceptance", Path(__file__).with_name("formation-5a-decision.py"))
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)

# Keep credentials out of Coordinator/Runtime subprocess environments and disk.
KEY = os.environ.pop("ATO_DECISION_JEV_API_KEY")
h.ENV.pop("ATO_DECISION_JEV_API_KEY", None)
RUN_ID = str(time.time_ns())
OUT = h.S4 / "live-jev" / RUN_ID
OUT.mkdir(parents=True)
h.OUT = OUT
h.LEDGER.close()
h.LEDGER = (OUT / "ledger.jsonl").open("a")
MODEL = os.environ.get("ATO_DECISION_JEV_MODEL", "jev-1.13.0")
# Published price checked 2026-09-26, https://docs.typesafe.ai/models.
# An estimate, not a billing receipt. Output tokens are free for this model.
INPUT_USD_PER_MILLION = 0.042 if MODEL == "jev-1.13.0" else None


def tree_digest(root):
    files = {p.relative_to(root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
             for p in sorted(root.rglob("*")) if p.is_file()}
    return hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()


def run_arm(name, mode, source):
    d.OWNER = f"live_{name}_{RUN_ID}"
    d.TOKEN = OUT / f"token-{name}"
    d.ensure_owner()
    h.OWNER, h.TOKEN = d.OWNER, d.TOKEN
    root = OUT / name
    root.mkdir()
    rt = h.runtime(name, "runtime", 2)
    requester = None
    try:
        rid = h.sql("SELECT id FROM runner_devices WHERE user_id=?", d.OWNER)[0]["id"]
        h.wait_for(lambda: h.sql("SELECT environment_id FROM runtime_environments WHERE runtime_id=?", rid),
                   "Runtime advertisement", timeout=120)
        env = dict(h.ENV, ATO_ACCEPTANCE_DECISION_POLICY="1,30000",
                   ATO_ACCEPTANCE_DECISION_PROVIDER=mode,
                   ATO_ACCEPTANCE_DECISION_LOG=str(root / "provider-calls.jsonl"),
                   ATO_ACCEPTANCE_SETTLE_SECS="900")
        if mode == "jev":
            env.update(ATO_DECISION_JEV_API_KEY=KEY, ATO_DECISION_JEV_MODEL=MODEL)
        sid = f"live{name}{RUN_ID}"
        start = time.monotonic()
        with (root / "status.json").open("w") as stdout, (root / "request.log").open("w") as stderr:
            requester = subprocess.Popen(
                [str(h.REQ), h.API, str(d.TOKEN), str(source), str(root / "work"), sid, "2",
                 str(root / "created.json"), str(h.ROUTES / "d-fail.toml"),
                 str(h.ROUTES / "d-good.toml")], env=env, stdout=stdout, stderr=stderr,
                start_new_session=True)
            rc = requester.wait(timeout=1000)
        elapsed = time.monotonic() - start
        result = json.loads((root / "status.json").read_text())
        calls = d.calls(root)
        live_calls = [c for c in calls if c["mode"] == "jev"]
        usages = [c.get("answer", {}).get("evidence", {}).get("usage") for c in live_calls]
        metered = all(isinstance(u, dict) and isinstance(u.get("input_tokens"), int)
                      and isinstance(u.get("output_tokens"), int) for u in usages)
        inputs = sum(u["input_tokens"] for u in usages) if metered else None
        outputs = sum(u["output_tokens"] for u in usages) if metered else None
        cost = (inputs * INPUT_USD_PER_MILLION / 1_000_000
                if inputs is not None and INPUT_USD_PER_MILLION is not None else None)
        row = {
            "arm": name, "search_id": sid, "satisfy_id": result["satisfy_id"],
            "fixture_sha256": tree_digest(source), "contract_ref": result["contract_ref"],
            "candidate_refs": [c["derivation_ref"] for c in result["search_state"]["frozen"]["candidates"]],
            "policy": result["search_state"]["frozen"]["policy"],
            "attempt_count": len(result["attempts"]), "elapsed_seconds": round(elapsed, 3),
            "provider_calls": len(live_calls), "local_decision_calls": len(calls),
            "input_tokens": inputs, "output_tokens": outputs,
            "estimated_cost_usd": cost, "price_usd_per_million_input": INPUT_USD_PER_MILLION,
            "price_source": "https://docs.typesafe.ai/models", "model": MODEL if live_calls else None,
            "final_outcome": result["status"], "termination_reason": result["termination_reason"],
            "attempts": [{"attempt_id": a["attempt_id"], "derivation_ref": a["derivation_ref"],
                          "status": a["status"],
                          "fully_satisfied": ((a.get("formation_attempt") or {}).get("receipt") or {}).get("fully_satisfied")}
                         for a in result["attempts"]],
            "decisions": result["decisions"], "budget_consumed": result["search_state"]["budget"],
            "calls": live_calls,
        }
        # Numeric ceilings, not time-varying reservations or absolute deadlines.
        search = h.sql("SELECT * FROM runtime_network_searches WHERE owner_user_id=? AND search_id=?",
                       d.OWNER, sid)[0]
        row["budget_limits"] = {k: v for k, v in search.items()
                                if k.startswith("max_") or k in ("deadline_seconds", "mode")}
        h.note(name, "measurement", **row)
        h.expect(rc == 0 and result["status"] == "satisfied", "verified final result", name)
        h.expect(len(calls) == 1 and len(result["decisions"]) == 1,
                 "one bounded decision", name)
        decision = result["decisions"][0]
        h.expect(decision["outcome"] == "chosen", "choice accepted by Coordinator", name)
        if mode == "jev":
            h.expect(len(live_calls) == 1 and metered, "live usage recorded", name)
            answer = live_calls[0]["answer"]
            h.expect(answer.get("choice_id") in live_calls[0]["offered"]
                     and answer["choice_id"] == decision["chosen_choice_id"]
                     and answer["evidence"]["model"] == MODEL,
                     "pinned live model selected an offered action", name)
        else:
            h.expect(decision["chosen_choice_id"] == decision["default_choice_id"],
                     "baseline used deterministic default", name)
        return row
    finally:
        if requester is not None:
            h.stop(requester)
        h.stop(rt)


if __name__ == "__main__":
    h.fixtures()
    source = OUT / "source"
    shutil.copytree(h.FIX / "notes", source)
    # One nonce shared by both arms avoids stale source rows on repeated runs.
    (source / "comparison.txt").write_text(RUN_ID + "\n")
    h.start_coordinator("live-jev")
    try:
        baseline = run_arm("deterministic", "fixed:0", source)
        jev = run_arm("jev", "jev", source)
        for key in ("fixture_sha256", "contract_ref", "candidate_refs", "policy", "budget_limits"):
            h.expect(baseline[key] == jev[key], f"identical {key}", "comparison")
        h.note("comparison", "ALL_PASSED", model=MODEL, arms=[baseline, jev])
        (OUT / "comparison.json").write_text(json.dumps([baseline, jev], indent=2) + "\n")
    finally:
        h.stop_coordinator()
        # Owner tokens are local harness credentials; do not retain this run's copies.
        for path in OUT.glob("token-*"):
            path.unlink()
