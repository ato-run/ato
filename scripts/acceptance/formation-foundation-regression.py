#!/usr/bin/env python3
"""Final foundation-stack regression on the Runtime Network: source Formation
for Static, a >32 MiB Static source, Python and Node through the Stage 4
Coordinator; then every source object deleted, the Coordinator restarted, and
each retained candidate replayed on a fresh Runtime with a fresh receipt.

Reuses formation-stage4-search.py (installed beside it as stage4_acceptance.py).
"""
import json, os, shutil, subprocess, time
import stage4_acceptance as h

OUT = h.S4 / "regression-network"
OUT.mkdir(parents=True, exist_ok=True)
h.OUT = OUT
LEDGER = open(OUT / "ledger.jsonl", "a")
h.LEDGER = LEDGER


def source(name):
    root = OUT / f"source-{name}-{time.time_ns()}"
    if name.startswith("static"):
        shutil.copytree(h.FIX / "static-page", root)
    elif name == "python":
        shutil.copytree(h.FIX / "notes", root)
    else:
        root.mkdir()
        (root / "app.js").write_text(
            "require('http').createServer((req,res)=>{res.writeHead(200);res.end('node pass')})"
            ".listen(8000,'0.0.0.0');\n")
        manifest = (h.FIX / "notes/capsule.toml").read_text()
        manifest = manifest.replace('name = "python"', 'name = "node"').replace(
            'version = "3.12.7"', 'version = "20.20.2"').replace(
            '["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "/app/app.py"]',
            '["node", "app.js"]')
        (root / "capsule.toml").write_text(manifest)
    if name == "static-40mib":
        for i in range(40):
            (root / f"bytes-{i:03}.bin").write_bytes(os.urandom(1024 * 1024))
    (root / "regression.txt").write_text(f"{name} {time.time_ns()}\n")
    return root


def form(name):
    case = OUT / name
    case.mkdir(exist_ok=True)
    src = source(name)
    rt = h.runtime(name, "runtime", 1)
    sid = f"regr{name.replace('-', '')}{time.time_ns() % 10**10}"
    p = subprocess.Popen(
        [str(h.REQ), h.API, str(h.TOKEN), str(src), str(case / "request-work"), sid, "2",
         str(case / "created.json"), str(src / "capsule.toml")],
        stdout=open(case / "status.json", "w"), stderr=open(case / "request.log", "w"),
        env=dict(h.ENV, ATO_ACCEPTANCE_SETTLE_SECS="900"), start_new_session=True)
    rc = p.wait(timeout=1200)
    h.stop(rt)
    status = json.loads((case / "status.json").read_text())
    route = status["verified_routes"][0]
    attempt = next(a for a in status["attempts"] if a["attempt_id"] == route["attempt_id"])
    result = {"rc": rc, "status": status["status"], "contract_ref": status["contract_ref"],
              "derivation_ref": route["derivation_ref"], "attempt": route["attempt_id"],
              "retained_ref": route["retained_ref"],
              "fully_satisfied": attempt["formation_attempt"]["receipt"]["fully_satisfied"],
              "charged": attempt.get("resource_charged"),
              "source_bytes": sum(f.stat().st_size for f in src.rglob("*") if f.is_file())}
    assert rc == 0 and status["status"] == "satisfied" and result["fully_satisfied"], result
    h.note(name, "source_formation_passed", **result)
    return result


def replay(name, formed):
    case = OUT / f"{name}-replay"
    case.mkdir(exist_ok=True)
    rt = h.runtime(f"{name}-replay", "runtime", 1)
    sid = f"regr{name.replace('-', '')}r{time.time_ns() % 10**10}"
    p = subprocess.run([str(h.REPLAY), h.API, str(h.TOKEN), formed["retained_ref"], sid],
                       stdout=open(case / "status.json", "w"),
                       stderr=open(case / "request.log", "w"), env=h.ENV, timeout=1200)
    h.stop(rt)
    status = json.loads((case / "status.json").read_text())
    attempt = status["attempts"][0]
    result = {"rc": p.returncode, "status": status["status"], "attempt": attempt["attempt_id"],
              "same_k": status["contract_ref"] == formed["contract_ref"],
              "same_d": attempt["derivation_ref"] == formed["derivation_ref"],
              "fresh_attempt": attempt["attempt_id"] != formed["attempt"],
              "fully_satisfied": attempt["formation_attempt"]["receipt"]["fully_satisfied"],
              "charged": attempt.get("resource_charged")}
    assert (p.returncode == 0 and status["status"] == "satisfied" and result["same_k"]
            and result["same_d"] and result["fresh_attempt"] and result["fully_satisfied"]
            and result["charged"]["stored_bytes"] == 0), result
    h.note(name, "retained_replay_passed", **result)
    return result


if __name__ == "__main__":
    h.start_coordinator("regression")
    results = {}
    try:
        names = ["static", "static-40mib", "python", "node"]
        for name in names:
            results[name] = {"source": form(name)}
        removed = h.command({"delete_sources": True})
        h.note("regression", "source_objects_deleted", **removed)
        h.restart("regression", "after_source_deletion")
        for name in names:
            results[name]["replay"] = replay(name, results[name]["source"])
    finally:
        h.stop_coordinator()
        (OUT / "results.json").write_text(json.dumps(results, indent=1, sort_keys=True))
