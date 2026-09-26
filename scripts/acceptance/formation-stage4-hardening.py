#!/usr/bin/env python3
"""Stage 4 review-hardening acceptance on the actual Coordinator (Miniflare
D1/R2 with the production Runtime Network routes, restarted as a separate
process), a real Rust Runtime and the Rust requester.

  H1  the Runtime is drained inside the ticket INSERT, i.e. after the Rust
      decision and after the Coordinator's own re-check: no ticket, no
      attempt/transfer reservation; undrained, the same search issues and passes.
  H2  D1 fails, its stored verifier receipts are inflated past the bounded
      authority's input limit, the Coordinator restarts, and the search still
      decides and issues D2, which passes.
  H3  the satisfied first-pass search of H2 is resent byte-identically by the
      requester: the Coordinator answers with the settled request, nothing new
      is stored or charged, and the requester accepts its route.
  H4  (Stage 4 CASE 6 re-run) the Runtime attests a non-disposable effect class,
      refuses it and records not_started: effect_policy_refused, D2 never issued.

Fault injection: one acceptance-only SQL trigger that drains the Runtime at
the ticket INSERT (H1), one that pauses the D2 INSERT to hold a restart point
and a SQL update that inflates stored receipt evidence (H2). Runs as its own
owner so the per-owner source quota of earlier runs is not involved.

Reuses formation-stage4-search.py (installed beside it as stage4_acceptance.py).
"""
import hashlib, json, secrets, subprocess, time
import stage4_acceptance as h

OWNER = "hardening_acceptance"
OUT = h.S4 / "hardening"
OUT.mkdir(parents=True, exist_ok=True)
h.OUT = OUT
h.LEDGER = open(OUT / "ledger.jsonl", "a")
TOKEN = h.S4 / "runner-token-hardening"


def ensure_owner():
    """A user and a Runtime token of this run's own, copied from the default
    acceptance owner's rows (same shape, new identity)."""
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


def runtime_id():
    digest = hashlib.sha256(TOKEN.read_text().strip().encode()).hexdigest()
    return h.sql("SELECT id FROM runner_devices WHERE token_hash=?", digest)[0]["id"]


def search_state(satisfy, sid):
    s = h.state("h", satisfy, sid)
    budget = h.sql("SELECT attempts_reserved, transfer_reserved, expanded_reserved, "
                   "attempts_used FROM runtime_network_searches WHERE owner_user_id=? "
                   "AND search_id=?", OWNER, sid)[0]
    return s, budget


def source(case):
    root = OUT / f"source-{case}-{time.time_ns()}"
    subprocess.run(["cp", "-r", str(h.FIX / "notes"), str(root)], check=True)
    (root / "hardening-case.txt").write_text(f"{case} {time.time_ns()}\n")
    return root


def submit(case, sid, src, routes, tag="request", extra_env=None):
    root = OUT / case / tag
    root.mkdir(parents=True, exist_ok=True)
    process = subprocess.Popen(
        [str(h.REQ), h.API, str(TOKEN), str(src), str(root / "work"), sid, "5",
         str(root / "created.json"), *map(str, routes)],
        stdout=open(root / "status.json", "w"), stderr=open(root / "request.log", "w"),
        env=dict(h.ENV, ATO_ACCEPTANCE_SETTLE_SECS="900", **(extra_env or {})),
        start_new_session=True)
    h.wait_for(lambda: (root / "created.json").exists() and (root / "created.json").read_text(),
               f"{case} request created", timeout=180, poll=0.2)
    created = json.loads((root / "created.json").read_text())
    h.note(case, "request_created", tag=tag, **created)
    return process, created, root


def finished(case, process, root):
    rc = process.wait(timeout=1200)
    text = (root / "status.json").read_text()
    result = json.loads(text) if text.strip() else None
    h.note(case, "requester_exited", rc=rc, status=result and result.get("status"),
           termination=result and result.get("termination_reason"))
    return rc, result


def h1():
    """Drain inside the ticket INSERT: nothing issued, nothing reserved."""
    c = "h1"
    sid = f"hard1{time.time_ns() % 10**10}"
    rid = runtime_id()
    h.sql("CREATE TRIGGER acc_drain_at_issue BEFORE INSERT ON satisfy_attempts "
          "WHEN NEW.satisfy_id IN (SELECT id FROM satisfy_requests WHERE search_id=?) "
          "BEGIN UPDATE runner_devices SET drained_at=NEW.created_at WHERE id=NEW.runtime_id; END"
          .replace("?", f"'{sid}'"))
    rt = h.runtime(c, "runtime", 1)
    try:
        # The Runtime has advertised before the request is created.
        h.wait_for(lambda: h.sql("SELECT 1 FROM runtime_availability WHERE runtime_id=?", rid),
                   "Runtime availability", timeout=120)
        req, created, root = submit(c, sid, source(c), [h.ROUTES / "d-good.toml"])
        satisfy = created["satisfy_id"]
        drained = h.wait_for(
            lambda: h.sql("SELECT drained_at FROM runner_devices WHERE id=?", rid)[0]["drained_at"],
            "drain injected at the ticket INSERT", timeout=120)
        time.sleep(10)  # the requester keeps polling; every poll advances
        s, budget = search_state(satisfy, sid)
        view = h.status_as(satisfy, TOKEN)
        reasons = [r for cand in view["candidates"] for r in cand["filter_reasons"]]
        h.expect(not s["attempts"] and budget["attempts_reserved"] == 0
                 and budget["transfer_reserved"] == 0 and view["status"] == "running"
                 and {"code": "runtime_drained"} in reasons,
                 "no ticket and no reservation after a drain at issue", c,
                 state=s, budget=budget)
        h.note(c, "no_ticket_after_drain_at_issue", satisfy_id=satisfy, drained_at=drained,
               attempts=len(s["attempts"]), budget=budget, filter_reasons=reasons,
               search_revision=s["search"]["revision"])
        h.sql("DROP TRIGGER acc_drain_at_issue")
        h.sql("UPDATE runner_devices SET drained_at=NULL WHERE id=?", rid)
        rc, result = finished(c, req, root)
        s, budget = search_state(satisfy, sid)
        h.expect(rc == 0 and result["status"] == "satisfied" and len(s["attempts"]) == 1
                 and budget["attempts_used"] == 1,
                 "undrained, exactly one ticket issued and verified", c, state=s)
        h.note(c, "passed_after_undrain", attempt=s["attempts"][0]["id"], budget=budget,
               contract=result["contract_ref"])
    finally:
        h.sql("DROP TRIGGER IF EXISTS acc_drain_at_issue")
        h.sql("UPDATE runner_devices SET drained_at=NULL WHERE id=?", rid)
        h.stop(rt)
    return {"search_id": sid, "satisfy_id": satisfy}


def h2():
    """Receipt history past the authority input limit does not block the search."""
    c = "h2"
    sid = f"hard2{time.time_ns() % 10**10}"
    src = source(c)
    routes = [h.ROUTES / "d-fail.toml", h.ROUTES / "d-good.toml"]
    req, created, root = submit(c, sid, src, routes)
    satisfy = created["satisfy_id"]
    # Hold every ticket after the first of this request (the D2 issue), so the
    # restart falls between D1's failure and D2's issue.
    h.sql("CREATE TRIGGER acc_pause_d2 BEFORE INSERT ON satisfy_attempts "
          f"WHEN NEW.satisfy_id='{satisfy}' AND EXISTS (SELECT 1 FROM satisfy_attempts "
          "WHERE satisfy_id=NEW.satisfy_id) BEGIN SELECT RAISE(IGNORE); END")
    rt = h.runtime(c, "runtime", 2)
    try:
        first = h.wait_for(lambda: h.state(c, satisfy, sid)["attempts"], "D1 ticket", timeout=120)
        d1 = first[0]
        h.wait_for(lambda: h.state(c, satisfy, sid)["attempts"][0]["status"] == "fail",
                   "D1 failed", timeout=600)
        receipt = {"kind": "contract_verification", "receipt": "r" * (1 << 20)}
        h.sql("UPDATE satisfy_attempts SET result_json=json_set(result_json,'$.verifier_receipts',"
              "json(?)) WHERE id=?", json.dumps([receipt, receipt]), d1["id"])
        size = h.sql("SELECT length(result_json) AS n FROM satisfy_attempts WHERE id=?",
                     d1["id"])[0]["n"]
        h.expect(size > 2 << 20, "stored receipt history exceeds 2 MiB", c, size=size)
        h.restart(c, "after_d1_failure_with_oversized_receipts")
        s, budget = search_state(satisfy, sid)
        h.expect(len(s["attempts"]) == 1 and budget["attempts_reserved"] == 0,
                 "D2 not yet issued", c, state=s)
        h.note(c, "d1_failed_receipts_inflated", attempt=d1["id"], result_json_bytes=size,
               failure=s["attempts"][0]["failure"])
        h.sql("DROP TRIGGER acc_pause_d2")
        rc, result = finished(c, req, root)
        s, budget = search_state(satisfy, sid)
        h.expect(rc == 0 and result["status"] == "satisfied" and len(s["attempts"]) == 2
                 and s["attempts"][1]["status"] == "pass"
                 and all("receipts" not in a for a in result["search_state"]["attempts"]),
                 "D2 issued and verified over oversized D1 receipt history", c, state=s)
        d2 = s["attempts"][1]
        h.note(c, "d2_passed", attempt=d2["id"], derivation=d2["derivation_ref"],
               contract=result["contract_ref"], budget=budget,
               fully_satisfied=next(a for a in result["attempts"] if a["attempt_id"] == d2["id"])
               ["formation_attempt"]["receipt"]["fully_satisfied"])
    finally:
        h.sql("DROP TRIGGER IF EXISTS acc_pause_d2")
        h.stop(rt)
    return {"search_id": sid, "satisfy_id": satisfy, "source": src, "routes": routes,
            "d2": d2["id"]}


def h3(prior):
    """Resend of the satisfied first-pass search: the settled request and its route."""
    c = "h3"
    sid = prior["search_id"]

    def counts():
        return {
            "requests": h.sql("SELECT count(*) AS n FROM satisfy_requests WHERE search_id=? "
                              "AND requested_by_user_id=?", sid, OWNER)[0]["n"],
            "attempts": h.sql("SELECT count(*) AS n FROM satisfy_attempts a JOIN satisfy_requests r "
                              "ON r.id=a.satisfy_id WHERE r.search_id=?", sid)[0]["n"],
            "sources": h.sql("SELECT count(*) AS n FROM runtime_network_sources WHERE owner_user_id=?",
                             OWNER)[0]["n"],
            "budget": h.sql("SELECT attempts_used, transfer_used, expanded_used, stored_used, revision "
                            "FROM runtime_network_searches WHERE owner_user_id=? AND search_id=?",
                            OWNER, sid)[0],
        }
    before = counts()
    h.restart(c, "before_resend")
    req, created, root = submit(c, sid, prior["source"], prior["routes"], tag="resend")
    rc, result = finished(c, req, root)
    after = counts()
    h.expect(created["satisfy_id"] == prior["satisfy_id"] and created.get("existing") is True
             and rc == 0 and result["status"] == "satisfied"
             and [r["attempt_id"] for r in result["verified_routes"]] == [prior["d2"]]
             and after == before,
             "resend answered with the settled request; requester accepted its route", c,
             created=created, before=before, after=after)
    h.note(c, "resend_answered_with_settled_request", satisfy_id=created["satisfy_id"],
           route_attempt=prior["d2"], before=before, after=after)
    return {"satisfy_id": created["satisfy_id"]}


def h4():
    """Proven-not-started policy refusal is its own termination reason."""
    c = "h4"
    sid = f"hard4{time.time_ns() % 10**10}"
    rt = h.runtime(c, "runtime", 2)
    try:
        req, created, root = submit(c, sid, source(c), [h.ROUTES / "d-nonrep.toml",
                                                        h.ROUTES / "d-good.toml"],
                                    extra_env={"ATO_ACCEPTANCE_DECLARED_EFFECTS": "pure"})
        rc, result = finished(c, req, root)
    finally:
        h.stop(rt)
    s, budget = search_state(created["satisfy_id"], sid)
    h.expect(result["termination_reason"] == "effect_policy_refused" and len(s["attempts"]) == 1
             and s["attempts"][0]["effects"] == "non-repeatable"
             and s["attempts"][0]["record"] == "not_started",
             "effect_policy_refused, D2 never issued", c, state=s)
    h.note(c, "effect_policy_refused", attempt=s["attempts"][0]["id"],
           failure=s["attempts"][0]["failure"], record=s["attempts"][0]["record"],
           status=result["status"], budget=budget)
    return {"satisfy_id": created["satisfy_id"], "attempt": s["attempts"][0]["id"]}


def status_as(satisfy_id, token_file):
    import urllib.request
    req = urllib.request.Request(f"{h.API}/v1/runtime-network/satisfy/{satisfy_id}",
                                 headers={"authorization": "Bearer " + token_file.read_text().strip()})
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.loads(response.read())


if __name__ == "__main__":
    h.status_as = status_as
    h.fixtures()
    h.start_coordinator("hardening")
    results = {}
    try:
        ensure_owner()
        # The Runtime serves with this run's token; the requester submits with it.
        h.TOKEN = TOKEN
        h.OWNER = OWNER
        import sys
        wanted = set(sys.argv[1:]) or {"h1", "h2", "h3", "h4"}
        if "h1" in wanted:
            results["h1"] = h1()
        if "h2" in wanted:
            results["h2"] = h2()
            results["h3"] = h3(results["h2"])
        if "h4" in wanted:
            results["h4"] = h4()
        h.note("hardening", "ALL_PASSED")
    finally:
        h.stop_coordinator()
        path = OUT / "results.json"
        merged = json.loads(path.read_text()) if path.exists() else {}
        merged.update(results)
        path.write_text(json.dumps(merged, indent=1, sort_keys=True, default=str))
