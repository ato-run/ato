#!/usr/bin/env python3
"""P0 20-app remeasurement on the foundation stack (a regression check, not
coverage work). Same pinned commits and the same hand-written routes as the
2026-09-23 benchmark; one first honest attempt per app against the Stage 4
Coordinator and a native Linux aarch64 Runtime; typed K0 only (the Browser
prompt is recorded as not run). Every result is classified by the foundation
layer of its first blocker.

Usage: formation-p0-remeasure.py BENCHMARK_JSON [SLUG ...]
"""
import json, shutil, subprocess, sys, time
import stage4_acceptance as h

import os, pathlib
# A separate owner for a rerun: the per-owner source object quota (0298) is
# deliberately not recycled, and earlier acceptance runs used the default one.
if os.environ.get("P0_TOKEN_FILE"):
    h.TOKEN = pathlib.Path(os.environ["P0_TOKEN_FILE"])
    h.OWNER = os.environ["P0_OWNER"]
OUT = h.S4 / "p0"
OUT.mkdir(parents=True, exist_ok=True)
h.OUT = OUT
h.LEDGER = open(OUT / "ledger.jsonl", "a")
AVAILABLE = {"node@20.20.2", "node@22.14.0", "python@3.12.7", "pnpm@9.15.4", "yarn@1.22.22"}


def slug(repo):
    return repo.split("/")[1].lower()


def fetch(app):
    root = OUT / "src" / slug(app["repo"])
    if (root / ".fetched").exists():
        return root, None
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    run = lambda *a: subprocess.run(a, cwd=root, capture_output=True, text=True, timeout=1800)
    steps = [("git", "init", "-q"), ("git", "remote", "add", "origin", f"https://github.com/{app['repo']}.git"),
             ("git", "fetch", "-q", "--depth", "1", "origin", app["commit_sha"]),
             ("git", "checkout", "-q", "FETCH_HEAD")]
    for step in steps:
        p = run(*step)
        if p.returncode != 0:
            return None, f"{' '.join(step)}: {p.stderr[-300:]}"
    shutil.rmtree(root / ".git")
    (root / ".fetched").write_text(app["commit_sha"])
    return root, None


def layer(code, stage):
    code = code or ""
    if code.startswith("source") or code in {"search_transfer_budget_exceeded", "search_expanded_budget_exceeded"}:
        return "source_transport"
    if stage == "publish" or code.startswith("artifact_store") or "publication" in code or "retained" in code:
        return "publication"
    if code in {"network_denied", "effect_policy"} or stage == "admission":
        return "runtime_capability"
    if stage in {"projection", "planning", "plan"} or code.startswith("projection") or code.startswith("lowering"):
        return "execution_lowering"
    if stage in {"preset", "authoring", "binding", "detect"}:
        return "d_authoring_detection"
    if stage in {"build", "provision", "dependency"} or "toolchain" in code or "build" in code:
        return "build_toolchain"
    if stage == "verification":
        return "k_verification"
    return f"other:{stage}:{code}"


def measure(app):
    s = slug(app["repo"])
    result = {"repo": app["repo"], "commit_sha": app["commit_sha"],
              "declared_runtimes": app["derivation"].get("declared_runtimes") or [],
              "browser_judgment": "not_run"}
    missing = [r for r in result["declared_runtimes"] if r not in AVAILABLE]
    root, error = fetch(app)
    if error:
        result.update(level=None, layer="source_transport", code="fetch_failed", detail=error)
        return result
    result["source_bytes"] = sum(f.stat().st_size for f in root.rglob("*") if f.is_file())
    route = OUT / "routes" / f"{s}.toml"
    route.parent.mkdir(exist_ok=True)
    route.write_text(app["derivation"]["route_toml"])
    case = OUT / s
    shutil.rmtree(case, ignore_errors=True)
    case.mkdir()
    rt = h.runtime(s, "runtime", 1)
    sid = f"p0{s.replace('-', '')}{time.time_ns() % 10**10}"
    settle = 240 if missing else 2700
    started = time.time()
    p = subprocess.Popen(
        [str(h.REQ), h.API, str(h.TOKEN), str(root), str(case / "request-work"), sid, "1",
         str(case / "created.json"), str(route)],
        stdout=open(case / "status.json", "w"), stderr=open(case / "request.log", "w"),
        env=dict(h.ENV, ATO_ACCEPTANCE_SETTLE_SECS=str(settle)), start_new_session=True)
    try:
        rc = p.wait(timeout=settle + 900)
    except subprocess.TimeoutExpired:
        h.stop(p)
        rc = "timeout"
    h.stop(rt)
    result["seconds"] = round(time.time() - started)
    created = (case / "created.json").read_text().strip() if (case / "created.json").exists() else ""
    if not created:
        text = (case / "request.log").read_text()[-600:]
        code = ("source_too_large" if "larger than" in text else
                "unsupported_step" if "projection" in text or "cannot execute" in text else
                "authoring" if "capsule" in text.lower() or "parse" in text.lower() else "requester_refused")
        result.update(level=None, code=code, detail=text.strip(),
                      layer={"source_too_large": "source_transport",
                             "unsupported_step": "execution_lowering",
                             "authoring": "d_authoring_detection"}.get(code, "other:requester"))
        return result
    satisfy = json.loads(created)["satisfy_id"]
    view = h.status(satisfy)
    result["satisfy_id"] = satisfy
    result["status"] = view["status"]
    result["termination_reason"] = view.get("termination_reason")
    attempts = view.get("attempts", [])
    if view["status"] == "satisfied":
        route_row = view["verified_routes"][0]
        result.update(level=5, layer="verified_route", attempt=route_row["attempt_id"],
                      retained_ref=route_row.get("retained_ref"))
        # Retained replay of what was just verified: a fresh Run, fresh receipt.
        rt = h.runtime(s, "replay", 1)
        r = subprocess.run([str(h.REPLAY), h.API, str(h.TOKEN), route_row["retained_ref"], sid + "r"],
                           stdout=open(case / "replay.json", "w"), stderr=open(case / "replay.log", "w"),
                           env=h.ENV, timeout=2400)
        h.stop(rt)
        replay = json.loads((case / "replay.json").read_text() or "{}")
        result["retained_replay"] = replay.get("status") if r.returncode == 0 else "failed"
    elif not attempts:
        reasons = sorted({c["code"] for cand in view.get("candidates", [])
                          for c in cand.get("filter_reasons", [])})
        result.update(level=None, layer="runtime_capability", code="no_admissible_runtime",
                      detail={"missing_toolchains": missing, "filter_reasons": reasons,
                              "search_action": view.get("search_events", [])[-1:] if view.get("search_events") else None})
    else:
        a = attempts[-1]
        failure = (a.get("failure") or {})
        fa = a.get("formation_attempt") or {}
        verification = fa.get("verification")
        result.update(attempt=a["attempt_id"], outcome=a.get("outcome") or a.get("status"),
                      code=failure.get("code"), stage=failure.get("stage"),
                      detail=(failure.get("message") or "")[:300])
        if a.get("status") == "unknown":
            result.update(level=0, layer="unknown")
        elif result["stage"] == "verification" and result["code"] != "candidate_not_observable":
            result.update(level=2, layer="k_verification")
        elif result["code"] == "candidate_not_observable":
            result.update(level=1, layer="k_verification")
        else:
            result.update(level=0, layer=layer(result["code"], result["stage"]))
        result["verification"] = verification
    # Leave nothing running for the next app.
    h.sql("UPDATE satisfy_requests SET status='stopped', stop_json=?, updated_at=? "
          "WHERE id=? AND status IN ('running','unknown')",
          json.dumps({"actor_user_id": h.OWNER, "note": "p0 remeasure: next app"}),
          time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()), satisfy)
    return result


if __name__ == "__main__":
    bench = json.load(open(sys.argv[1]))
    wanted = set(sys.argv[2:])
    h.start_coordinator("p0")
    path = OUT / "results.json"
    results = json.loads(path.read_text()) if path.exists() else {}
    try:
        for app in bench["apps"]:
            s = slug(app["repo"])
            if wanted and s not in wanted:
                continue
            h.note(s, "begin")
            results[s] = measure(app)
            h.note(s, "measured", **{k: v for k, v in results[s].items() if k != "verification"})
            path.write_text(json.dumps(results, indent=1, sort_keys=True))
    finally:
        h.stop_coordinator()
