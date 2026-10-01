#!/usr/bin/env python3
"""5b-a actual isolated Coordinator + Rust Runtime/requester acceptance.

Install beside stage4_acceptance.py and formation-5a-decision.py. The local
Coordinator must already contain migration 0303 and the matching Rust WASM.
No remote D1, staging or production services are used. Optional c_live reads
ATO_GENERATION_JEV_API_KEY from this process only; one model invocation.
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

import stage4_acceptance as h

spec = importlib.util.spec_from_file_location(
    "decision_acceptance", Path(__file__).with_name("formation-5a-decision.py"))
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)
h.API_DIR = h.HOME / "stage5b-api"
h.COMMANDS = h.API_DIR / "commands"
KEY = os.environ.pop("ATO_GENERATION_JEV_API_KEY", None)
h.ENV.pop("ATO_GENERATION_JEV_API_KEY", None)
RUN_ID = str(time.time_ns())
OUT = h.S4 / "typed-generation" / RUN_ID
OUT.mkdir(parents=True)
h.OUT = OUT
h.LEDGER.close()
h.LEDGER = (OUT / "ledger.jsonl").open("a")
CANARY = "SOURCE_SECRET_CANARY_NEVER_SEND_5B"


def setup(case):
    d.OWNER = f"typed_{case}_{RUN_ID}"
    d.TOKEN = OUT / f"token-{case}"
    d.ensure_owner()
    h.OWNER, h.TOKEN = d.OWNER, d.TOKEN
    root = OUT / case
    root.mkdir()
    source = root / "source"
    shutil.copytree(h.FIX / "notes", source)
    (source / "bad.py").write_text(
        "from http.server import SimpleHTTPRequestHandler, HTTPServer\n"
        "HTTPServer(('0.0.0.0', 8000), SimpleHTTPRequestHandler).serve_forever()\n")
    (source / "private-source.txt").write_text(CANARY + RUN_ID)
    route = root / "base.toml"
    route.write_text((source / "capsule.toml").read_text().replace("/app/app.py", "/app/bad.py"))
    return root, source, route


def start(case, mode=None, enabled=True, entries=None, exact=False):
    root, source, route = setup(case)
    rt = h.runtime(case, "runtime", 3)
    rid = h.sql("SELECT id FROM runner_devices WHERE user_id=?", d.OWNER)[0]["id"]
    h.wait_for(lambda: h.sql("SELECT environment_id FROM runtime_environments WHERE runtime_id=?", rid),
               "Runtime registered", timeout=120)
    env = dict(h.ENV, ATO_ACCEPTANCE_SETTLE_SECS="180")
    if exact:
        env["ATO_ACCEPTANCE_EXACT_RUNTIME"] = rid
    if enabled:
        env["ATO_ACCEPTANCE_GENERATION_ENTRYPOINTS"] = json.dumps(entries or {"entry_app": "app.py"})
    if mode:
        env["ATO_ACCEPTANCE_GENERATION_PROVIDER"] = mode
        env["ATO_ACCEPTANCE_GENERATION_LOG"] = str(root / "generation-calls.jsonl")
    if mode == "jev":
        assert KEY, "live key missing"
        env["ATO_GENERATION_JEV_API_KEY"] = KEY
    sid = f"typed{case}{RUN_ID}"
    began = time.monotonic()
    requester = subprocess.Popen(
        [str(h.REQ), h.API, str(d.TOKEN), str(source), str(root / "work"), sid, "2",
         str(root / "created.json"), str(route)],
        env=env, stdout=(root / "status.json").open("w"),
        stderr=(root / "request.log").open("w"), start_new_session=True)
    h.wait_for(lambda: (root / "created.json").exists() and (root / "created.json").read_text(),
               "request created", timeout=120)
    satisfy = json.loads((root / "created.json").read_text())["satisfy_id"]
    return root, sid, satisfy, requester, rt, began


def post(satisfy, suffix, payload):
    req = urllib.request.Request(f"{h.API}/v1/runtime-network/satisfy/{satisfy}/generation{suffix}",
                                 data=json.dumps(payload).encode(), headers={
                                     "authorization": "Bearer " + d.TOKEN.read_text().strip(),
                                     "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as res:
            return res.status, json.load(res)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def rows(sid):
    return h.sql("SELECT * FROM satisfy_generations WHERE owner_user_id=? AND search_id=?", d.OWNER, sid)


def finish(case, run, expected="admitted"):
    root, sid, satisfy, requester, rt, began = run
    try:
        rc = requester.wait(timeout=240)
        result = json.loads((root / "status.json").read_text())
        recorded = rows(sid)
        calls_path = root / "generation-calls.jsonl"
        calls = [json.loads(line) for line in calls_path.read_text().splitlines()] if calls_path.exists() else []
        report = {"search_id": sid, "satisfy_id": satisfy, "contract_ref": result["contract_ref"],
                  "attempts": [{"id": a["attempt_id"], "d": a["derivation_ref"], "status": a["status"]}
                               for a in result["attempts"]],
                  "outcome": result["status"], "generation_outcome": recorded[0]["outcome"] if recorded else None,
                  "elapsed_seconds": round(time.monotonic() - began, 3), "calls": calls,
                  "budget": result["search_state"]["budget"],
                  "policy": result["search_state"]["frozen"]["policy"],
                  "initial_candidates": result["search_state"]["frozen"]["candidates"]}
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        h.note(case, "observed", **report)
        h.expect(rc == 0, "requester accepted settlement", case)
        if expected == "admitted":
            h.expect(result["status"] == "satisfied" and len(result["attempts"]) == 2
                     and result["attempts"][0]["status"] == "fail"
                     and result["attempts"][1]["formation_attempt"]["receipt"]["fully_satisfied"],
                     "D1 FAIL to Dnew same K PASS", case)
            h.expect(len(result["search_state"]["frozen"]["candidates"]) == 1
                     and result["attempts"][1]["derivation_ref"] != result["attempts"][0]["derivation_ref"],
                     "Dnew was not an original candidate", case)
        else:
            h.expect(result["status"] == "unsatisfied" and len(result["attempts"]) == 1,
                     "no extra execution without admitted D", case)
        if result["search_state"]["frozen"]["policy"]["runtime_constraint"]["kind"] == "exact":
            required = result["search_state"]["frozen"]["policy"]["runtime_constraint"]["runtime_id"]
            h.expect(all(a["runtime_id"] == required for a in result["attempts"]),
                     "every attempt remains on exact authorized Runtime", case)
        if expected is None:
            h.expect(not recorded, "no policy no generation", case)
        else:
            h.expect(len(recorded) == 1 and recorded[0]["outcome"] == expected,
                     "one durable generation outcome", case)
            h.expect(CANARY not in recorded[0]["input_json"]
                     and "app.py" not in recorded[0]["input_json"]
                     and "capsule_toml" not in recorded[0]["input_json"],
                     "provider projection excludes source and paths", case)
        h.expect(len(calls) <= 1, "at most one provider invocation", case)
        h.note(case, "passed", **report)
        return report
    finally:
        h.stop(requester)
        h.stop(rt)


def c0():
    return finish("c0", start("c0", enabled=False), None)


def c1():
    return finish("c1", start("c1", "fixed:entry_app"))


def c_failure():
    return finish("c_failure", start("c_failure", "error"), "provider_error")


def c_declined():
    result = finish("c_declined", start("c_declined", "decline"), "declined")
    recorded = rows(result["search_id"])[0]
    provenance = json.loads(recorded["provenance_json"])
    h.expect(provenance == result["calls"][0]["answer"]["provenance"]
             and provenance["model"] == "fixed-decline",
             "decline retains bounded usage/provenance without a D", "c_declined")
    return result


def c_duplicate():
    return finish("c_duplicate", start("c_duplicate", "fixed:entry_bad", entries={"entry_bad": "bad.py"}), "duplicate")


def c_exact():
    return finish("c_exact", start("c_exact", "fixed:entry_app", exact=True))


def c_timeout():
    return finish("c_timeout", start("c_timeout"), "timeout")


def c_live():
    return finish("c_live", start("c_live", "jev"))


def c_restart_concurrent():
    case = "c_restart_concurrent"
    run = start(case)
    root, sid, satisfy, requester, rt, _ = run
    held = False
    try:
        view = h.wait_for(lambda: (lambda v: v if v.get("generation_point") else None)(
            d.status_as(satisfy, d.TOKEN)), "generation point", timeout=120)
        point = view["generation_point"]
        claim_results = []
        threads = [threading.Thread(target=lambda: claim_results.append(
            post(satisfy, "/claim", {"revision": point["revision"]}))) for _ in range(6)]
        for t in threads: t.start()
        for t in threads: t.join()
        wins = [body for code, body in claim_results if code == 200]
        h.expect(len(wins) == 1 and all(code in (200, 409) for code, _ in claim_results),
                 "exactly one generation claim", case)
        revision = wins[0]["revision"]
        # Hold just the generated ticket, after the original attempt has failed.
        h.sql("CREATE TRIGGER generation_acceptance_hold BEFORE INSERT ON satisfy_attempts "
              f"WHEN NEW.satisfy_id='{satisfy}' BEGIN SELECT RAISE(IGNORE); END")
        held = True
        body = {"revision": revision,
                "draft": {"schema": "ato.formation-derivation-draft/1", "operation": "python_script", "entrypoint_id": "entry_app"},
                "provenance": {"provider": "acceptance", "model": "fixed", "prompt_version": "acceptance/1",
                               "usage": {"input_tokens": 0, "output_tokens": 0}}}
        # Unknown fields cannot become execution authority.
        for key, value in {"contract": {}, "permission": "all", "argv": ["sh", "-c", "bad"],
                           "url": "https://invalid.example", "source_patch": "bad", "secret": "read"}.items():
            bad = json.loads(json.dumps(body))
            bad["draft"][key] = value
            h.expect(post(satisfy, "", bad)[0] == 400, f"reject {key}", case)
        submitted = []
        threads = [threading.Thread(target=lambda: submitted.append(post(satisfy, "", body))) for _ in range(6)]
        for t in threads: t.start()
        for t in threads: t.join()
        h.expect(sum(code == 200 for code, _ in submitted) == 1
                 and all(code in (200, 409) for code, _ in submitted), f"one completion: {submitted}", case)
        before = rows(sid)
        h.restart(case, "admitted_before_generated_ticket")
        after = rows(sid)
        h.expect(before == after and len(after) == 1,
                 "restart reuses exact generation/provenance row", case)
        h.sql("DROP TRIGGER generation_acceptance_hold")
        held = False
        return finish(case, run)
    finally:
        if held: h.sql("DROP TRIGGER IF EXISTS generation_acceptance_hold")
        h.stop(requester)
        h.stop(rt)


if __name__ == "__main__":
    h.start_coordinator("typed-generation")
    results = {}
    try:
        for case in sys.argv[1:] or ["c0", "c1", "c_failure", "c_duplicate", "c_restart_concurrent"]:
            results[case] = globals()[case]()
        if "c1" in results and "c_live" in results:
            baseline, live = results["c1"], results["c_live"]
            for field in ("contract_ref", "policy", "initial_candidates", "budget"):
                h.expect(baseline[field] == live[field], f"paired {field} equality", "comparison")
            h.note("comparison", "same_scope_budget_verified", baseline="c1", live="c_live")
        h.note("typed-generation", "ALL_PASSED", cases=list(results))
    finally:
        h.stop_coordinator()
        (OUT / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        for path in OUT.glob("token-*"): path.unlink()
