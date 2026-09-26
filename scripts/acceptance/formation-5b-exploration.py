#!/usr/bin/env python3
"""Formation 5a-b actual acceptance: finite exploration actions (Attempt |
Inspect | Stop) over durable decision points, on the actual Coordinator
(Miniflare D1/R2 with the production Runtime Network routes, restarted as a
separate process), a real Rust Runtime and the Rust requester
(`formation_search`).

  B0  no policy: the Stage 4 behaviour (D1 fails, D2 passes), no decision rows
  B1  attempt vs attempt: the provider picks D2 over the frozen default (5a-a)
  B2  Inspect chosen: attempt 0, evidence durable; Coordinator restart; a new
      decision_seq opens; Attempt chosen; PASS   (also B5, B6)
  B3  Stop chosen: ticket 0, status stopped, termination_reason
      decision_stopped, not a K failure
  B4  after a chosen Inspect the provider fails: deterministic default under
      the same budget
  B7  concurrent answers and evidence: exactly one answer wins; no duplicate
      action or evidence
  B8  provider-visible payload: canary Runtime facts never appear, inspect and
      stop carry no facts, and evidence is a bounded projection

D1 = d-fail (serves no /health: a known K failure), D2 = d-good. Runs as its
own owner. Reuses formation-stage4-search.py (installed as stage4_acceptance.py).
"""
import hashlib, json, secrets, subprocess, sys, threading, time, urllib.error, urllib.request
import os
import stage4_acceptance as h

OWNER = os.environ.get("DECISION_OWNER", "exploration_acceptance")
OUT = h.S4 / "decision-5b"
OUT.mkdir(parents=True, exist_ok=True)
h.OUT = OUT
h.LEDGER = open(OUT / "ledger.jsonl", "a")
TOKEN = h.S4 / ("runner-token-explore" + ("" if OWNER == "exploration_acceptance" else "-" + OWNER))


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
    sid = sid or f"exp{case}{time.time_ns() % 10**10}"
    root = OUT / case
    root.mkdir(parents=True, exist_ok=True)
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


def status_as(satisfy_id):
    req = urllib.request.Request(f"{h.API}/v1/runtime-network/satisfy/{satisfy_id}",
                                 headers={"authorization": "Bearer " + TOKEN.read_text().strip()})
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.loads(response.read())


def post_decision(satisfy_id, seq, choice_id=None, fallback=None):
    body = {"seq": seq}
    if choice_id is not None:
        body["choice_id"] = choice_id
    if fallback is not None:
        body["fallback"] = fallback
    req = urllib.request.Request(
        f"{h.API}/v1/runtime-network/satisfy/{satisfy_id}/decisions",
        data=json.dumps(body).encode(), method="POST",
        headers={"authorization": "Bearer " + TOKEN.read_text().strip(),
                 "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def decisions(sid):
    return h.sql("SELECT seq, attempt_seq, outcome, chosen_choice, default_choice, choices_json, "
                 "opened_at, decided_at FROM satisfy_decisions WHERE owner_user_id=? "
                 "AND search_id=? ORDER BY seq", OWNER, sid)


def chosen_action(row):
    for c in json.loads(row["choices_json"]):
        if c["choice_id"] == row["chosen_choice"]:
            return c["action"]
    return None


def evidence(sid):
    return h.sql("SELECT kind, target_ref, decision_seq, search_revision, result_json, "
                 "recorded_at FROM satisfy_evidence WHERE owner_user_id=? AND search_id=?",
                 OWNER, sid)


def calls(root):
    path = root / "provider-calls.jsonl"
    return [json.loads(l) for l in path.read_text().splitlines()] if path.exists() else []


def route_refs(sid):
    row = h.sql("SELECT frozen_json FROM runtime_network_searches WHERE owner_user_id=? "
                "AND search_id=?", OWNER, sid)[0]
    return [c["derivation_ref"] for c in json.loads(row["frozen_json"])["candidates"]]


def attempts_order(result):
    return [a["derivation_ref"] for a in result["attempts"]]


def choice_by(view, predicate):
    return next((c for c in view["decision_point"]["choices"] if predicate(c["action"])), None)


def is_attempt(action):
    return action["kind"] == "attempt"


def is_inspect(action):
    return action["kind"] == "inspect"


def is_stop(action):
    return action["kind"] == "stop"


def b0():
    c = "b0"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"])
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    order = attempts_order(r)
    refs = route_refs(sid)
    h.expect(rc == 0 and r["status"] == "satisfied" and order == refs
             and not decisions(sid) and not evidence(sid)
             and "decision" not in r["search_state"]["frozen"]["policy"],
             "no policy: Stage 4 order, no decision point", c, order=order)
    h.note(c, "passed", order=order)
    return {"satisfy_id": satisfy}


def b1():
    c = "b1"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="fixed:1", policy="2,30000")
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    refs = route_refs(sid)
    action = chosen_action(d[0])
    h.expect(rc == 0 and r["status"] == "satisfied" and order == [refs[1]]
             and len(d) == 1 and d[0]["outcome"] == "chosen"
             and action and action["kind"] == "attempt" and action["derivation_ref"] == refs[1]
             and r["attempts"][0]["formation_attempt"]["receipt"]["fully_satisfied"]
             and len(calls(root)) == 1,
             "attempt vs attempt: D2 chosen, PASS, one call", c, decisions=d, order=order)
    h.note(c, "passed", decision=d[0], attempt=r["attempts"][0]["attempt_id"])
    return {"satisfy_id": satisfy}


def b2():
    """Inspect first: evidence lands with no attempt; the Coordinator restarts;
    a new decision seq opens; an Attempt is chosen; the route passes."""
    c = "b2"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="silent", policy="4,120000")
        view = h.wait_for(lambda: (lambda v: v if v.get("decision_point") else None)
                          (status_as(satisfy)), "decision point", timeout=120, poll=0.2)
        inspect = choice_by(view, is_inspect)
        status, body = post_decision(satisfy, 0, choice_id=inspect["choice_id"])
        h.expect(status == 200, "inspect answer accepted", c, status=status, body=body)
        # Evidence recorded with zero attempts, then a NEW decision seq opens.
        h.wait_for(lambda: (lambda v: v if v.get("decision_point")
                            and v["decision_point"]["seq"] == 1 else None)(status_as(satisfy)),
                   "second decision point", timeout=120, poll=0.2)
        ev = evidence(sid)
        d = decisions(sid)
        view = status_as(satisfy)
        h.expect(len(d) == 2 and d[0]["outcome"] == "chosen"
                 and chosen_action(d[0])["kind"] == "inspect"
                 and d[1]["outcome"] is None and len(ev) == 1
                 and ev[0]["decision_seq"] == 0 and not view["attempts"]
                 and d[0]["attempt_seq"] == 0 and d[1]["attempt_seq"] == 0,
                 "evidence durable, seq 1 open, attempt count still 0 (B5)", c,
                 decisions=d, evidence=ev)
        # Coordinator restart: the recorded evidence is reused, never re-run,
        # and the open point survives untouched.
        h.restart(c, "evidence_recorded_point_open")
        ev2 = evidence(sid)
        view = status_as(satisfy)
        h.expect(ev2 == ev and view["decision_point"]["seq"] == 1
                 and not view["attempts"],
                 "restart kept evidence and the open point (B6)", c,
                 evidence_before=ev, evidence_after=ev2)
        # The attempt the provider picks now lands and passes.
        attempt = next(x for x in view["decision_point"]["choices"]
                       if is_attempt(x["action"])
                       and x["action"]["derivation_ref"] == route_refs(sid)[1])
        status, body = post_decision(satisfy, 1, choice_id=attempt["choice_id"])
        h.expect(status == 200, "attempt answer accepted", c, status=status, body=body)
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    refs = route_refs(sid)
    h.expect(rc == 0 and r["status"] == "satisfied" and order == [refs[1]]
             and [x["outcome"] for x in d] == ["chosen", "chosen"]
             and chosen_action(d[1])["kind"] == "attempt"
             and r["attempts"][0]["formation_attempt"]["receipt"]["fully_satisfied"]
             and len(evidence(sid)) == 1,
             "inspect -> restart -> new seq -> attempt -> PASS", c, decisions=d, order=order)
    h.note(c, "passed", decisions=d, evidence=evidence(sid),
           attempt=r["attempts"][0]["attempt_id"])
    return {"satisfy_id": satisfy, "decisions": d, "evidence": evidence(sid)}


def b3():
    c = "b3"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="kind:stop", policy="2,30000")
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    h.expect(rc == 0 and r["status"] == "stopped"
             and r["termination_reason"] == "decision_stopped"
             and r["stop"]["kind"] == "decision_stopped"
             and not r["attempts"] and not evidence(sid)
             and len(d) == 1 and d[0]["outcome"] == "chosen"
             and chosen_action(d[0])["kind"] == "stop",
             "stop: 0 tickets, decision_stopped, not a K failure", c,
             decisions=d, status=r["status"], termination=r["termination_reason"])
    h.note(c, "passed", decision=d[0], status=r["status"])
    return {"satisfy_id": satisfy}


def b4():
    """After a chosen Inspect the provider fails: the same deterministic
    default runs under the same budget (script answers only seq 0)."""
    c = "b4"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="seq:0=2", policy="4,30000")
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    order = attempts_order(r)
    refs = route_refs(sid)
    h.expect(rc == 0 and r["status"] == "satisfied" and order == refs
             and [x["outcome"] for x in d] == ["chosen", "invalid", "invalid"]
             and chosen_action(d[0])["kind"] == "inspect"
             and len(evidence(sid)) == 1 and len(calls(root)) == 3
             and r["search_state"]["budget"]["decisions_used"] == 3,
             "provider failure after inspect: deterministic default, same budget", c,
             decisions=d, order=order)
    h.note(c, "passed", decisions=d, attempts=[a["attempt_id"] for a in r["attempts"]])
    return {"satisfy_id": satisfy}


def b7():
    """Concurrent answers settle exactly one; the released action is the
    winner's; evidence and decisions stay singular."""
    c = "b7"
    rt = h.runtime(c, "runtime", 3)
    try:
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="silent", policy="6,120000")
        view = h.wait_for(lambda: (lambda v: v if v.get("decision_point") else None)
                          (status_as(satisfy)), "decision point", timeout=120, poll=0.2)
        offered = [x["choice_id"] for x in view["decision_point"]["choices"]]
        codes = []
        def post(cid, at):
            time.sleep(max(0, at - time.time()))
            codes.append((post_decision(satisfy, 0, choice_id=cid)[0], cid))
        start = time.time() + 0.3
        threads = [threading.Thread(target=post, args=(cid, start + 0.02 * i))
                   for i, cid in enumerate(offered[:6])]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        h.note(c, "race_settled", codes=codes)
        ok = sum(1 for code, _ in codes if code == 200)
        h.expect(ok == 1 and all(code in (200, 409) for code, _ in codes),
                 "exactly one answer lands", c, codes=codes)
        # Drive the rest with fallbacks until the search settles.
        for _ in range(30):
            view = status_as(satisfy)
            if view["status"] != "running":
                break
            point = view.get("decision_point")
            if point:
                post_decision(satisfy, point["seq"], fallback="provider_error")
            time.sleep(0.5)
        rc, r = finished(c, req, root)
    finally:
        h.stop(rt)
    d = decisions(sid)
    ev = evidence(sid)
    winner = next((cid for code, cid in codes if code == 200), None)
    action = chosen_action(d[0])
    won = next(x["action"] for x in json.loads(d[0]["choices_json"])
               if x["choice_id"] == winner)
    h.expect(rc == 0 and action == won and len(d) >= 1
             and len({(e["kind"], e["target_ref"]) for e in ev}) == len(ev)
             and r["status"] in ("satisfied", "stopped")
             and r["termination_reason"] in ("verified", "decision_stopped",
                                             "candidates_exhausted", "budget_exhausted"),
             "one outcome, released action is the winner's, no duplicate evidence", c,
             decisions=d, evidence=ev, status=r["status"])
    h.note(c, "passed", winner_action=won, status=r["status"],
           terminations=[x["outcome"] for x in d], evidence=len(ev))
    return {"satisfy_id": satisfy, "winner": won, "status": r["status"]}


def b8():
    """The provider-visible payload: canary facts never appear, non-attempt
    choices carry no Runtime facts, evidence is a bounded projection."""
    c = "b8"
    rid = h.sql("SELECT id FROM runner_devices WHERE token_hash=?",
                hashlib.sha256(TOKEN.read_text().strip().encode()).hexdigest())[0]["id"]
    rt = h.runtime(c, "runtime", 3)
    injected = None
    try:
        profile = h.wait_for(lambda: h.sql(
            "SELECT e.current_facts_ref AS ref, p.facts_json AS facts FROM runtime_environments e "
            "JOIN runtime_capability_profiles p ON p.facts_ref=e.current_facts_ref "
            "WHERE e.runtime_id=?", rid), "Runtime profile", timeout=120)[0]
        injected = profile
        h.sql("UPDATE runtime_capability_profiles SET facts_json=json_set(facts_json,"
              "'$.\"runtime.hostname\"','build-host-canary.internal',"
              "'$.\"toolchain.secret_token\"','s3cr3t-canary') WHERE facts_ref=?", profile["ref"])
        # seq0 -> first inspect (records evidence); later points -> default.
        req, satisfy, sid, root = submit(c, [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"],
                                         provider="seq:0=2", policy="4,30000")
        rc, r = finished(c, req, root)
    finally:
        if injected:
            h.sql("UPDATE runtime_capability_profiles SET facts_json=? WHERE facts_ref=?",
                  injected["facts"], injected["ref"])
        h.stop(rt)
    seen = calls(root)
    all_facts = json.loads(injected["facts"])
    ok = len(seen) >= 1
    for call in seen:
        text = json.dumps(call)
        ok &= "canary" not in text
        for cid, facts in call["view_facts"].items():
            required = {x["fact"] for x in (call["requirements"][cid] or [])}
            ok &= set(facts) <= required and len(facts) < len(all_facts)
        for entry in call.get("evidence") or []:
            etext = json.dumps(entry)
            ok &= "canary" not in etext and "attempt_id" not in etext and "message" not in etext
    h.expect(ok and rc == 0 and r["status"] == "satisfied" and len(evidence(sid)) == 1,
             "provider sees requirement-scoped facts and bounded evidence only", c, calls=seen)
    h.note(c, "passed", provider_view_facts=seen[0]["view_facts"],
           evidence=seen[-1].get("evidence"), runtime_fact_count=len(all_facts) + 2)
    return {"satisfy_id": satisfy}


if __name__ == "__main__":
    h.status_as = status_as
    h.fixtures()
    h.start_coordinator("exploration")
    results = {}
    try:
        ensure_owner()
        h.TOKEN = TOKEN
        h.OWNER = OWNER
        wanted = sys.argv[1:] or ["b0", "b1", "b2", "b3", "b4", "b7", "b8"]
        for name in wanted:
            results[name] = globals()[name]()
        h.note("exploration", "ALL_PASSED", cases=wanted)
    finally:
        h.stop_coordinator()
        path = OUT / "results.json"
        merged = json.loads(path.read_text()) if path.exists() else {}
        merged.update(results)
        path.write_text(json.dumps(merged, indent=1, sort_keys=True, default=str))
