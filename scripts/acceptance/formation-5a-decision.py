#!/usr/bin/env python3
"""Formation 5a actual acceptance: a requester-side DecisionProvider over
finite AllowedChoices, on the actual Coordinator (Miniflare D1/R2 with the
production Runtime Network routes, restarted as a separate process), a real
Rust Runtime and the Rust requester (`formation_search` with its acceptance
provider: fixed index / out-of-set label / silence).

  A0  no policy: the Stage 4 behaviour (D1 fails, D2 passes), no decision rows
  A1  the provider picks D2 over the frozen default D1: one attempt, D2 PASS
  A2  an out-of-set answer: recorded out_of_set, default D1 first, then D2
  A3  silence, Coordinator restart while waiting: timeout fallback, default D1
  A4  max_decisions 1: the second point never opens; default thereafter
  A5  Coordinator restart after the answer was recorded and before the issue:
      the recorded choice is issued, no point re-opens, the provider is asked once

D1 = d-fail (serves no /health: a known K failure), D2 = d-good. Runs as its
own owner. Reuses formation-stage4-search.py (installed as stage4_acceptance.py).
"""
import hashlib, json, secrets, subprocess, sys, time
import stage4_acceptance as h

OWNER = "decision_acceptance"
OUT = h.S4 / "decision-5a"
OUT.mkdir(parents=True, exist_ok=True)
h.OUT = OUT
h.LEDGER = open(OUT / "ledger.jsonl", "a")
TOKEN = h.S4 / "runner-token-decision"


def ensure_owner():
    if TOKEN.exists():
        return
    user = h.sql('SELECT * FROM "user" WHERE id=?', h.OWNER)[0]
    user.update(id=OWNER, email=f"{OWNER}@acceptance.invalid", name=OWNER)
    h.sql(f'INSERT INTO "user" ({",".join(user)}) VALUES ({",".join("?" * len(user))})',
          *user.values())
    runner = h.sql("SELECT * FROM runner_devices WHERE user_id=? ORDER BY created_at LIMIT 1",
                   h.OWNER)[0]
    token = "ato_rnr_" + secrets.token_hex(32)
    runner.update(id="rnr_" + secrets.token_hex(12), user_id=OWNER, display_name=OWNER,
                  token_hash=hashlib.sha256(token.encode()).hexdigest(), drained_at=None,
                  revoked_at=None, status="active")
    h.sql(f"INSERT INTO runner_devices ({','.join(runner)}) VALUES ({','.join('?' * len(runner))})",
          *runner.values())
    TOKEN.write_text(token + "\n")
    TOKEN.chmod(0o600)
    h.note("setup", "owner_created", owner=OWNER, runtime_id=runner["id"])


def source(case):
    root = OUT / f"source-{case}-{time.time_ns()}"
    subprocess.run(["cp", "-r", str(h.FIX / "notes"), str(root)], check=True)
    (root / "decision-case.txt").write_text(f"{case} {time.time_ns()}\n")
    return root


def submit(case, routes, provider=None, policy=None, sid=None):
    sid = sid or f"dec{case}{time.time_ns() % 10**10}"
    root = OUT / case
    root.mkdir(parents=True, exist_ok=True)
    # A rerun of a case must never read the previous run's outputs.
    for stale in ("created.json", "status.json", "provider-calls.jsonl"):
        (root / stale).unlink(missing_ok=True)
    env = dict(h.ENV, ATO_ACCEPTANCE_SETTLE_SECS="900")
    if policy:
        env["ATO_ACCEPTANCE_DECISION_POLICY"] = policy
    if provider:
        env["ATO_ACCEPTANCE_DECISION_PROVIDER"] = provider
        env["ATO_ACCEPTANCE_DECISION_LOG"] = str(root / "provider-calls.jsonl")
    process = subprocess.Popen(
        [str(h.REQ), h.API, str(TOKEN), str(source(case)), str(root / "work"), sid, "5",
         str(root / "created.json"), *map(str, routes)],
        stdout=open(root / "status.json", "w"), stderr=open(root / "request.log", "w"),
        env=env, start_new_session=True)
    h.wait_for(lambda: (root / "created.json").exists() and (root / "created.json").read_text(),
               f"{case} request created", timeout=180, poll=0.2)
    created = json.loads((root / "created.json").read_text())
    h.note(case, "request_created", search_id=sid, satisfy_id=created["satisfy_id"],
           provider=provider, policy=policy)
    return process, created["satisfy_id"], sid, root


def finished(case, process, root):
    rc = process.wait(timeout=1200)
    text = (root / "status.json").read_text()
    result = json.loads(text) if text.strip() else None
    h.note(case, "requester_exited", rc=rc, status=result and result.get("status"),
           termination=result and result.get("termination_reason"))
    return rc, result


def decisions(sid):
    return h.sql("SELECT seq, outcome, chosen_choice, default_choice, choices_json, opened_at, "
                 "decided_at FROM satisfy_decisions WHERE owner_user_id=? AND search_id=? "
                 "ORDER BY seq", OWNER, sid)


def calls(root):
    path = root / "provider-calls.jsonl"
    return [json.loads(l) for l in path.read_text().splitlines()] if path.exists() else []


def route_ref(result, name):
    """The DerivationRef of a route file, from the frozen candidate order."""
    return result["search_state"]["frozen"]["candidates"][name]["derivation_ref"]


def attempts_order(result):
    return [a["derivation_ref"] for a in result["attempts"]]


def a0():
    c = "a0"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"])
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    order = attempts_order(r)
    h.expect(rc == 0 and r["status"] == "satisfied" and len(order) == 2
             and order == [route_ref(r, 0), route_ref(r, 1)] and not decisions(sid)
             and "decision" not in r["search_state"]["frozen"]["policy"],
             "no policy: Stage 4 order, no decision point", c, order=order)
    h.note(c, "passed", attempts=[a["attempt_id"] for a in r["attempts"]], order=order)
    return {"satisfy_id": satisfy, "attempts": [a["attempt_id"] for a in r["attempts"]]}


def a1():
    c = "a1"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="fixed:1", policy="2,30000")
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    passed = r["attempts"][0]
    h.expect(rc == 0 and r["status"] == "satisfied" and order == [route_ref(r, 1)]
             and len(d) == 1 and d[0]["outcome"] == "chosen"
             and d[0]["default_choice"] != d[0]["chosen_choice"]
             and r["search_state"]["budget"].get("decisions_used") == 1
             and passed["formation_attempt"]["receipt"]["fully_satisfied"]
             and len(calls(root)) == 1,
             "provider reordered: D2 first and only, PASS, one call", c, decisions=d, order=order)
    h.note(c, "passed", attempt=passed["attempt_id"], decision=d[0], calls=len(calls(root)),
           contract=r["contract_ref"])
    return {"satisfy_id": satisfy, "attempt": passed["attempt_id"], "decision": d[0]}


def a2():
    c = "a2"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="out_of_set", policy="2,30000")
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    h.expect(rc == 0 and r["status"] == "satisfied" and order == [route_ref(r, 0), route_ref(r, 1)]
             and [x["outcome"] for x in d] == ["out_of_set"]
             and not r["search_state"]["budget"].get("decisions_used"),
             "out_of_set recorded; default D1 then D2", c, decisions=d, order=order)
    h.note(c, "passed", decisions=d, attempts=[a["attempt_id"] for a in r["attempts"]])
    return {"satisfy_id": satisfy, "decisions": d}


def a3():
    c = "a3"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="silent", policy="2,15000")
        h.wait_for(lambda: decisions(sid), "decision point opened", timeout=120)
        h.restart(c, "while_decision_open")
        d = decisions(sid)
        s = h.state(c, satisfy, sid)
        h.expect(len(d) == 1 and d[0]["outcome"] is None and not s["attempts"],
                 "open point and no ticket survive restart", c, decisions=d, state=s)
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    h.expect(rc == 0 and r["status"] == "satisfied" and d[0]["outcome"] == "timeout"
             and order[0] == route_ref(r, 0) and len(calls(root)) == 1,
             "timeout fallback took the default D1 first", c, decisions=d, order=order)
    h.note(c, "passed", decision=d[0], attempts=[a["attempt_id"] for a in r["attempts"]])
    return {"satisfy_id": satisfy, "decision": d[0]}


def a4():
    c = "a4"
    rt = h.runtime(c, "runtime", 4)
    try:
        req, satisfy, sid, root = submit(
            c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-fail-b.toml", h.ROUTES / "d-good.toml"],
            provider="fixed:1", policy="1,30000")
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    h.expect(rc == 0 and r["status"] == "satisfied"
             and order == [route_ref(r, 1), route_ref(r, 0), route_ref(r, 2)]
             and [x["outcome"] for x in d] == ["chosen"] and len(calls(root)) == 1
             and r["search_state"]["budget"]["decisions_used"] == 1,
             "one decision, then the default order without a point", c, decisions=d, order=order)
    h.note(c, "passed", decisions=d, attempts=[a["attempt_id"] for a in r["attempts"]])
    return {"satisfy_id": satisfy}


def a5():
    c = "a5"
    rt = h.runtime(c, "runtime", 3)
    sid = f"deca5{time.time_ns() % 10**10}"
    # Hold every ticket of this search, from before its first request, so the
    # restart falls between the recorded answer and the issue it decides.
    h.sql("CREATE TRIGGER acc_hold_issue BEFORE INSERT ON satisfy_attempts "
          "WHEN NEW.satisfy_id IN (SELECT id FROM satisfy_requests "
          f"WHERE search_id='{sid}') BEGIN SELECT RAISE(IGNORE); END")
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="fixed:1", policy="2,30000", sid=sid)
        h.wait_for(lambda: decisions(sid) and decisions(sid)[0]["outcome"], "answer recorded",
                   timeout=120)
        h.restart(c, "after_answer_before_issue")
        view = h.status_as(satisfy, TOKEN)
        h.expect(view["decision_point"] is None and len(decisions(sid)) == 1
                 and not view["attempts"], "no point re-opens after restart", c, view=view["decisions"])
        h.sql("DROP TRIGGER acc_hold_issue")
        rc, r = finished(c, req, root)
    finally:
        h.sql("DROP TRIGGER IF EXISTS acc_hold_issue")
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    h.expect(rc == 0 and r["status"] == "satisfied" and order == [route_ref(r, 1)]
             and len(d) == 1 and len(calls(root)) == 1,
             "recorded choice issued after restart; provider asked once", c, decisions=d)
    h.note(c, "passed", decision=d[0], attempt=r["attempts"][0]["attempt_id"],
           calls=len(calls(root)))
    return {"satisfy_id": satisfy}


def status_as(satisfy_id, token_file):
    import urllib.request
    req = urllib.request.Request(f"{h.API}/v1/runtime-network/satisfy/{satisfy_id}",
                                 headers={"authorization": "Bearer " + token_file.read_text().strip()})
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.loads(response.read())


if __name__ == "__main__":
    h.status_as = status_as
    h.fixtures()
    h.start_coordinator("decision")
    results = {}
    try:
        ensure_owner()
        h.TOKEN = TOKEN
        h.OWNER = OWNER
        wanted = sys.argv[1:] or ["a0", "a1", "a2", "a3", "a4", "a5"]
        for name in wanted:
            results[name] = globals()[name]()
        h.note("decision", "ALL_PASSED", cases=wanted)
    finally:
        h.stop_coordinator()
        path = OUT / "results.json"
        merged = json.loads(path.read_text()) if path.exists() else {}
        merged.update(results)
        path.write_text(json.dumps(merged, indent=1, sort_keys=True, default=str))
