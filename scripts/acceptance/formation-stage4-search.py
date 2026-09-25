#!/usr/bin/env python3
"""Stage 4 actual acceptance: a real Coordinator (Miniflare D1/R2 with the
production Runtime Network routes, restarted as a separate process), real Rust
Runtimes and the Rust requester, for known-D search over one frozen K.

Fault injection is limited to: SQL triggers that pause exactly one insert or
claim (to hold a restart point), a Runtime whose attempt journal directory is
read-only (its history cannot be established -> UNKNOWN), and a requester whose
effect hint is wrong. The shared Docker/host services are not touched.

Usage: stage4_acceptance.py [CASE ...]   (default: all)
"""
import json, os, pathlib, shutil, signal, subprocess, sys, time, urllib.request

HOME = pathlib.Path.home() / "formation-foundation"
S4 = HOME / "stage4"
API_DIR = HOME / "stage4-api"
ATO = S4 / "ato"
TOKEN = S4 / "runner-token"
REQ = HOME / "target/debug/examples/formation_search"
REPLAY = HOME / "target/debug/examples/retained_replay"
API = "http://127.0.0.1:19444"
NODE = "/opt/ato/toolchains/node/22.14.0/bin"
PY = "/opt/ato/toolchains/python/3.12.7/bin/python3"
OUT = S4 / "acceptance"
FIX = HOME / "3d/src/apps/formation-worker/fixtures/runtime-network"
OWNER = "source_acceptance"
OUT.mkdir(parents=True, exist_ok=True)
(S4 / "scratch").mkdir(exist_ok=True)
ENV = dict(os.environ, TMPDIR=str(S4 / "scratch"))
LEDGER = open(OUT / "ledger.jsonl", "a")


def note(case, event, **fields):
    record = {"at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "case": case,
              "event": event, **fields}
    LEDGER.write(json.dumps(record, sort_keys=True) + "\n")
    LEDGER.flush()
    print(json.dumps(record, sort_keys=True), flush=True)


# ── the Coordinator process ────────────────────────────────────────────────
coordinator = None
restarts = 0


def start_coordinator(tag):
    global coordinator
    log = open(OUT / f"coordinator-{tag}.log", "w")
    coordinator = subprocess.Popen(
        ["node", "coordinator.mjs"], cwd=API_DIR, stdout=log, stderr=subprocess.STDOUT,
        env=dict(ENV, PATH=NODE + ":" + ENV["PATH"]), start_new_session=True)
    deadline = time.time() + 90
    while time.time() < deadline:
        if "ready" in (OUT / f"coordinator-{tag}.log").read_text():
            return
        assert coordinator.poll() is None, f"coordinator {tag} exited"
        time.sleep(0.2)
    raise RuntimeError(f"coordinator {tag} did not become ready")


def stop_coordinator():
    global coordinator
    if coordinator and coordinator.poll() is None:
        os.killpg(coordinator.pid, signal.SIGTERM)
        coordinator.wait(timeout=30)
    coordinator = None


def restart(case, point):
    global restarts
    restarts += 1
    stop_coordinator()
    start_coordinator(f"{case}-{restarts}")
    note(case, "coordinator_restarted", point=point, restart=restarts)


COMMANDS = API_DIR / "commands"


def command(obj):
    COMMANDS.mkdir(exist_ok=True)
    name = f"{time.time_ns()}"
    (COMMANDS / f"{name}.in.json").write_text(json.dumps(obj))
    out = COMMANDS / f"{name}.out.json"
    deadline = time.time() + 30
    while not out.exists():
        assert time.time() < deadline, f"command {obj} not served"
        time.sleep(0.05)
    time.sleep(0.05)
    result = json.loads(out.read_text())
    if isinstance(result, dict) and "error" in result:
        raise RuntimeError(result["error"])
    return result


def sql(query, *params):
    return command({"sql": query, "params": list(params)})["results"]


def state(case, satisfy_id, search_id):
    """The durable rows a restart must preserve."""
    attempts = sql("SELECT id, derivation_ref, status, claimed_at IS NOT NULL AS claimed, "
                   "fence, unknown_resolved_at IS NOT NULL AS resolved, "
                   "json_extract(result_json,'$.failure.code') AS failure, "
                   "json_extract(result_json,'$.attestation.attempt_record') AS record, "
                   "json_extract(result_json,'$.attestation.effects') AS effects "
                   "FROM satisfy_attempts WHERE satisfy_id=? ORDER BY created_at, id", satisfy_id)
    search = sql("SELECT revision, attempts_used, attempts_reserved, transfer_used, "
                 "expanded_used, stored_used, frozen_json IS NOT NULL AS frozen, "
                 "length(frozen_json) AS frozen_len FROM runtime_network_searches "
                 "WHERE owner_user_id=? AND search_id=?", OWNER, search_id)[0]
    request = sql("SELECT status, termination_reason FROM satisfy_requests WHERE id=?",
                  satisfy_id)[0]
    routes = sql("SELECT attempt_id, derivation_ref FROM verified_routes WHERE satisfy_id=?",
                 satisfy_id)
    return {"attempts": attempts, "search": search, "request": request, "routes": routes}


def status(satisfy_id):
    """The owner's view (also advances the request, as the requester's poll does)."""
    req = urllib.request.Request(f"{API}/v1/runtime-network/satisfy/{satisfy_id}",
                                 headers={"authorization": "Bearer " + TOKEN.read_text().strip()})
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.loads(response.read())


# ── Runtimes and requesters ────────────────────────────────────────────────
def runtime(case, tag, attempts, readonly_journal=False):
    root = OUT / case / tag
    root.mkdir(parents=True, exist_ok=True)
    if readonly_journal:
        records = root / "out" / "attempt-records"
        records.mkdir(parents=True, exist_ok=True)
        records.chmod(0o555)
    log = open(root / "runtime.log", "w")
    process = subprocess.Popen(
        [str(ATO), "runtime-network", "serve", "--api", API, "--token-file", str(TOKEN),
         "--work-root", str(root / "work"), "--out", str(root / "out"),
         "--max-attempts", str(attempts)],
        stdout=log, stderr=subprocess.STDOUT, env=ENV, start_new_session=True)
    note(case, "runtime_started", tag=tag, pid=process.pid, readonly_journal=readonly_journal)
    return process


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
    return process.returncode


def source_for(case):
    """A source of its own per case: the 3d state this Coordinator continues
    had its source objects deleted behind its back, and a byte-identical
    source would reuse that row."""
    source = OUT / f"source-{case}-{time.time_ns()}"
    shutil.copytree(FIX / "notes", source)
    (source / "stage4-case.txt").write_text(f"{case} {time.time_ns()}\n")
    return source


def requester(case, search_id, max_attempts, routes, extra_env=None, settle=600):
    root = OUT / case
    root.mkdir(parents=True, exist_ok=True)
    source = source_for(case)
    env = dict(ENV, ATO_ACCEPTANCE_SETTLE_SECS=str(settle), **(extra_env or {}))
    process = subprocess.Popen(
        [str(REQ), API, str(TOKEN), str(source), str(root / "request-work"), search_id,
         str(max_attempts), str(root / "created.json"), *map(str, routes)],
        stdout=open(root / "status.json", "w"), stderr=open(root / "request.log", "w"),
        env=env, start_new_session=True)
    deadline = time.time() + 120
    while not (root / "created.json").exists() or not (root / "created.json").read_text():
        assert process.poll() is None, (root / "request.log").read_text()[-2000:]
        assert time.time() < deadline, "request not created"
        time.sleep(0.2)
    created = json.loads((root / "created.json").read_text())
    note(case, "request_created", satisfy_id=created["satisfy_id"], search_id=search_id)
    return process, created["satisfy_id"]


def settled(case, process, timeout=900):
    rc = process.wait(timeout=timeout)
    text = (OUT / case / "status.json").read_text()
    result = json.loads(text) if text.strip() else None
    note(case, "requester_exited", rc=rc,
         status=result and result.get("status"),
         termination=result and result.get("termination_reason"))
    return rc, result


def wait_for(predicate, what, timeout=600, poll=1.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(poll)
    raise AssertionError(f"timed out waiting for {what}")


# ── fixtures: one source, routes over one K ───────────────────────────────
ROUTES = OUT / "routes"


def fixtures():
    ROUTES.mkdir(exist_ok=True)
    good = (FIX / "notes/capsule.toml").read_text()
    good_argv = '["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "/app/app.py"]'
    assert good_argv in good
    # D1: serves the tree, has no /health -> K observation fails (known failure).
    fail = good.replace(good_argv, f'["{PY}", "-B", "-m", "http.server", "8000", "--directory", "/app"]')
    fail_b = good.replace(good_argv, f'["{PY}", "-B", "-m", "http.server", "8000", "--bind", "0.0.0.0", "--directory", "/app"]')
    nonrep = (FIX / "notes-non-repeatable/capsule.toml").read_text()
    for name, text in {"d-good.toml": good, "d-fail.toml": fail, "d-fail-b.toml": fail_b,
                       "d-nonrep.toml": nonrep}.items():
        (ROUTES / name).write_text(text)


def search_id(case):
    # The Coordinator's search id format: opaque, url-safe.
    return f"s4{case.lower().replace('_', '')}{time.time_ns() % 10**10}"


def expect(condition, message, case, **context):
    if not condition:
        note(case, "FAILED", message=message, **context)
        raise AssertionError(message)


# ── CASES ─────────────────────────────────────────────────────────────────
def case1():
    """D1 known fail -> restart -> D2 PASS; restart points 1, 2, 4/5, 7."""
    c = "case1"
    sid = search_id(c)
    req, satisfy = requester(c, sid, 5, [ROUTES / "d-fail.toml", ROUTES / "d-good.toml"])
    # Point 1: search created, no Runtime advertised yet -> nothing issued.
    restart(c, "1_after_creation")
    s = state(c, satisfy, sid)
    expect(s["search"]["frozen"] and not s["attempts"], "created search persisted", c, state=s)
    frozen_len = s["search"]["frozen_len"]
    # Point 2: a ticket reserved and not yet claimed. Pause claims only.
    sql("CREATE TRIGGER s4_pause_claim BEFORE UPDATE OF status ON satisfy_attempts "
        "WHEN NEW.status='claimed' BEGIN SELECT RAISE(IGNORE); END")
    rt = runtime(c, "runtime", 2)
    wait_for(lambda: state(c, satisfy, sid)["attempts"], "D1 ticket issued")
    restart(c, "2_ticket_reserved_before_claim")
    s = state(c, satisfy, sid)
    expect(len(s["attempts"]) == 1 and s["attempts"][0]["status"] == "pending"
           and s["search"]["attempts_reserved"] == 1, "reserved ticket survives restart", c, state=s)
    d1 = s["attempts"][0]
    # Points 4/5: hold exactly the D2 ticket INSERT so the restart falls between
    # D1's failure and D2's issue.
    sql("DROP TRIGGER s4_pause_claim")
    sql("CREATE TRIGGER s4_pause_d2 BEFORE INSERT ON satisfy_attempts "
        "WHEN NEW.derivation_ref != ? BEGIN SELECT RAISE(IGNORE); END".replace("?", f"'{d1['derivation_ref']}'"))
    wait_for(lambda: state(c, satisfy, sid)["attempts"][0]["status"] == "fail", "D1 failed", timeout=600)
    restart(c, "4_5_after_d1_failure_before_d2_issue")
    s = state(c, satisfy, sid)
    expect(len(s["attempts"]) == 1 and s["attempts"][0]["status"] == "fail"
           and s["search"]["attempts_used"] == 1 and s["search"]["attempts_reserved"] == 0,
           "D1 failure and budget persisted, D2 not issued", c, state=s)
    note(c, "d1_failed", attempt=d1["id"], failure=s["attempts"][0]["failure"],
         record=s["attempts"][0]["record"], search_revision=s["search"]["revision"])
    sql("DROP TRIGGER s4_pause_d2")
    rc, result = settled(c, req)
    s = state(c, satisfy, sid)
    expect(rc == 0 and result["status"] == "satisfied" and result["termination_reason"] == "verified",
           "D2 PASS verified and requester accepted", c, result=result and result.get("status"))
    d2 = s["attempts"][1]
    expect(d2["id"] != d1["id"] and d2["derivation_ref"] != d1["derivation_ref"]
           and d2["status"] == "pass" and s["search"]["attempts_used"] == 2
           and [r["attempt_id"] for r in s["routes"]] == [d2["id"]]
           and s["search"]["frozen_len"] == frozen_len,
           "fresh D2 attempt, cumulative budget, one route, frozen search unchanged", c, state=s)
    receipt = next(a for a in result["attempts"] if a["attempt_id"] == d2["id"])
    note(c, "d2_passed", attempt=d2["id"], derivation=d2["derivation_ref"],
         contract=result["contract_ref"],
         fully_satisfied=receipt["formation_attempt"]["receipt"]["fully_satisfied"],
         attempts=[a["id"] for a in s["attempts"]], budget=s["search"])
    # Point 7: after the PASS receipt was accepted.
    restart(c, "7_after_pass_accepted")
    again = status(satisfy)
    s2 = state(c, satisfy, sid)
    expect(again["status"] == "satisfied" and len(s2["routes"]) == 1
           and len(s2["attempts"]) == 2 and s2["search"]["attempts_used"] == 2,
           "no duplicate route/attempt/charge after restart", c, state=s2)
    stop(rt)
    return {"search_id": sid, "satisfy_id": satisfy, "d1": d1["id"], "d2": d2["id"]}


def case2():
    """D1 UNKNOWN -> restart -> no D2 -> owner resolution -> restart -> D2 PASS."""
    c = "case2"
    sid = search_id(c)
    req, satisfy = requester(c, sid, 5, [ROUTES / "d-fail.toml", ROUTES / "d-good.toml"])
    rt = runtime(c, "runtime-a", 1, readonly_journal=True)
    wait_for(lambda: any(a["status"] == "unknown" for a in state(c, satisfy, sid)["attempts"]),
             "D1 UNKNOWN", timeout=300)
    s = state(c, satisfy, sid)
    d1 = s["attempts"][0]
    note(c, "d1_unknown", attempt=d1["id"], record=d1["record"], request=s["request"])
    code = stop(rt)
    # Point 8: restart while UNKNOWN. Nothing further may be issued, even with a
    # healthy Runtime polling for work.
    restart(c, "8_during_unknown")
    healthy = runtime(c, "runtime-b", 1)
    time.sleep(20)
    view = status(satisfy)
    s = state(c, satisfy, sid)
    expect(len(s["attempts"]) == 1 and s["attempts"][0]["status"] == "unknown"
           and view["status"] == "unknown" and view["termination_reason"] is None,
           "UNKNOWN held across restart, no D2", c, state=s, view=view["status"])
    action = view["search_state"]
    # Physical cessation evidence: the Runtime that held the attempt has exited.
    evidence = (f"runtime pid {rt.pid} exited with code {code}; its attempt journal refused "
                f"the start (history unavailable), so the D1 derivation never launched")
    resolution = command({"resolve": {
        "satisfy_id": satisfy, "attempt_id": d1["id"], "owner": OWNER,
        "resolution": {"action": "resolve", "resolution": "no_effect_confirmed",
                       "note": "Stage 4 acceptance: D1 attempt refused before start",
                       "execution_stop": {"kind": "runtime_terminated", "evidence": evidence}}}})
    note(c, "owner_resolved", attempt=d1["id"], http=resolution["status"])
    expect(resolution["status"] == 200, "owner resolution accepted", c, resolution=resolution)
    # Point 9: restart directly after the resolution.
    restart(c, "9_after_owner_resolution")
    rc, result = settled(c, req)
    s = state(c, satisfy, sid)
    d2 = s["attempts"][1]
    expect(rc == 0 and result["status"] == "satisfied" and d2["status"] == "pass"
           and s["attempts"][0]["status"] == "unknown" and s["attempts"][0]["resolved"]
           and s["search"]["attempts_used"] == 2,
           "D2 PASS after resolution; D1 kept UNKNOWN+resolved, budget not refunded", c, state=s)
    stop(healthy)
    note(c, "d2_passed", attempt=d2["id"], d1=d1["id"], budget=s["search"])
    return {"search_id": sid, "satisfy_id": satisfy, "d1": d1["id"], "d2": d2["id"]}


def case3():
    """Retained D replay PASS without source objects; route usable after restart."""
    c = "case3"
    sid = search_id(c) + "src"
    rt = runtime(c, "runtime-a", 1)
    req, satisfy = requester(c, sid, 2, [ROUTES / "d-good.toml"])
    rc, result = settled(c, req)
    stop(rt)
    route = result["verified_routes"][0]
    retained = route["retained_ref"]
    note(c, "source_formation_passed", attempt=route["attempt_id"], retained_ref=retained)
    removed = command({"delete_sources": True})
    note(c, "source_objects_deleted", **removed)
    restart(c, "6_after_retained_registered_and_sources_deleted")
    outcomes = []
    for n in range(2):
        rsid = search_id(c) + f"r{n}"
        rt = runtime(c, f"replay-{n}", 1)
        root = OUT / c / f"replay-{n}"
        p = subprocess.run([str(REPLAY), API, str(TOKEN), retained, rsid],
                           stdout=open(root / "status.json", "w"),
                           stderr=open(root / "request.log", "w"), env=ENV, timeout=600)
        stop(rt)
        replay = json.loads((root / "status.json").read_text())
        attempt = replay["attempts"][0]
        expect(p.returncode == 0 and replay["status"] == "satisfied"
               and replay["contract_ref"] == result["contract_ref"]
               and attempt["derivation_ref"] == route["derivation_ref"]
               and attempt["attempt_id"] != route["attempt_id"]
               and attempt["formation_attempt"]["receipt"]["fully_satisfied"]
               and attempt["resource_charged"]["stored_bytes"] == 0,
               "fresh retained replay PASS, same K/D, new attempt/receipt", c, run=n)
        note(c, "retained_replay_passed", run=n, attempt=attempt["attempt_id"],
             search_action=replay.get("search_state", {}).get("frozen", {}).get("candidates", [{}])[0].get("materialization"))
        outcomes.append(attempt["attempt_id"])
        restart(c, f"after_replay_{n}")
        view = status(replay["satisfy_id"])
        expect(view["status"] == "satisfied" and view["verified_routes"],
               "replayed route still usable after restart", c)
    return {"source_attempt": route["attempt_id"], "replays": outcomes, "retained_ref": retained}


def case4():
    """Budget exhaustion is budget_exhausted, not candidate exhaustion."""
    c = "case4"
    sid = search_id(c)
    rt = runtime(c, "runtime", 1)
    req, satisfy = requester(c, sid, 1, [ROUTES / "d-fail.toml", ROUTES / "d-good.toml"])
    rc, result = settled(c, req)
    stop(rt)
    s = state(c, satisfy, sid)
    expect(result["status"] == "exhausted" and result["termination_reason"] == "budget_exhausted"
           and len(s["attempts"]) == 1, "budget_exhausted with an untried D left", c, state=s)
    return {"satisfy_id": satisfy, "termination": result["termination_reason"]}


def case5():
    """Every D known-fails: candidates_exhausted."""
    c = "case5"
    sid = search_id(c)
    rt = runtime(c, "runtime", 2)
    req, satisfy = requester(c, sid, 5, [ROUTES / "d-fail.toml", ROUTES / "d-fail-b.toml"])
    # Point 3: restart directly after the claim, while the Runtime executes;
    # its result is delivered to the restarted Coordinator.
    wait_for(lambda: any(a["claimed"] for a in state(c, satisfy, sid)["attempts"]),
             "D1 claimed", poll=0.1)
    restart(c, "3_directly_after_claim")
    rc, result = settled(c, req)
    stop(rt)
    s = state(c, satisfy, sid)
    expect(result["status"] == "unsatisfied" and result["termination_reason"] == "candidates_exhausted"
           and len(s["attempts"]) == 2 and all(a["status"] == "fail" for a in s["attempts"]),
           "candidates_exhausted after every D failed", c, state=s)
    return {"satisfy_id": satisfy, "termination": result["termination_reason"]}


def case6():
    """A route whose real effect class is not disposable: effect_unknown, no fallback."""
    c = "case6"
    sid = search_id(c)
    rt = runtime(c, "runtime", 2)
    req, satisfy = requester(c, sid, 5, [ROUTES / "d-nonrep.toml", ROUTES / "d-good.toml"],
                             extra_env={"ATO_ACCEPTANCE_DECLARED_EFFECTS": "pure"})
    rc, result = settled(c, req)
    stop(rt)
    s = state(c, satisfy, sid)
    expect(result["termination_reason"] == "effect_unknown" and len(s["attempts"]) == 1
           and s["attempts"][0]["effects"] == "non-repeatable",
           "effect_unknown, D2 never issued", c, state=s)
    return {"satisfy_id": satisfy, "attempt": s["attempts"][0], "termination": result["termination_reason"]}


def case7():
    """Concurrent advance from three Runtimes and parallel polls: one ticket per step."""
    c = "case7"
    sid = search_id(c)
    runtimes = [runtime(c, f"runtime-{n}", 2) for n in range(3)]
    req, satisfy = requester(c, sid, 5, [ROUTES / "d-fail.toml", ROUTES / "d-good.toml"])
    import threading
    stop_polls = threading.Event()
    errors = []

    def poll():
        while not stop_polls.is_set():
            try:
                status(satisfy)
            except Exception as error:  # a racing poll may lose; it must not duplicate
                errors.append(str(error))
    threads = [threading.Thread(target=poll) for _ in range(8)]
    for thread in threads:
        thread.start()
    rc, result = settled(c, req)
    stop_polls.set()
    for thread in threads:
        thread.join()
    for process in runtimes:
        stop(process)
    s = state(c, satisfy, sid)
    per_d = {}
    for a in s["attempts"]:
        per_d[a["derivation_ref"]] = per_d.get(a["derivation_ref"], 0) + 1
    expect(result["status"] == "satisfied" and len(s["attempts"]) == 2
           and sorted(per_d.values()) == [1, 1] and s["search"]["attempts_used"] == 2
           and s["search"]["attempts_reserved"] == 0 and len(s["routes"]) == 1,
           "one ticket per D and one reservation each under concurrency", c, state=s)
    return {"attempts": [a["id"] for a in s["attempts"]], "poll_errors": len(errors)}


CASES = {"1": case1, "2": case2, "3": case3, "4": case4, "5": case5, "6": case6, "7": case7}

if __name__ == "__main__":
    fixtures()
    start_coordinator("initial")
    # A clean slate from earlier runs of this harness: its pause triggers, and
    # requests it left open (their tickets would otherwise reach this run).
    for name in ["s4_pause_claim", "s4_pause_d2"]:
        sql(f"DROP TRIGGER IF EXISTS {name}")
    stale = sql("SELECT id FROM satisfy_requests WHERE status IN ('running','unknown')")
    for row in stale:
        sql("UPDATE satisfy_requests SET status='stopped', stop_json=?, updated_at=? WHERE id=?",
            json.dumps({"actor_user_id": OWNER, "note": "stage4 harness reset"}),
            time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()), row["id"])
    note("setup", "stale_requests_stopped", ids=[row["id"] for row in stale])
    results = {}
    try:
        for key in sys.argv[1:] or list(CASES):
            note(f"case{key}", "begin")
            results[key] = CASES[key]()
            note(f"case{key}", "PASS", **{"result": results[key]})
    finally:
        stop_coordinator()
        (OUT / "results.json").write_text(json.dumps(results, indent=1, sort_keys=True))
