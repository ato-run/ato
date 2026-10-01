#!/usr/bin/env python3
"""6b-B: remeasure the six workspace-refusal archives, twice, with no model.

A: existing known-D coverage_baseline (CandidateProducer OFF).
B: workspace_proposal fixed-producer driver, per the plan's preregistered
   selection rule. Network denied in both. Archive hashes precede any run.
"""
import argparse, hashlib, json, os, subprocess
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(args, log, env):
    with log.open("x") as stream:
        subprocess.run(list(map(str, args)), env=env, stdout=stream,
                       stderr=subprocess.STDOUT, timeout=600, check=True)


def main():
    p = argparse.ArgumentParser()
    for name in ("plan", "checkout", "archives", "bin", "output"):
        p.add_argument("--" + name, type=Path, required=True)
    a = p.parse_args()
    plan = json.loads(a.plan.read_text())
    assert plan["schema"] == "ato.formation-node-static-workspace-remeasurement/1"
    assert plan["network"] == "denied" and plan["model_calls"] == 0
    assert len(plan["applications"]) == 6
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=a.checkout, text=True).strip()
    assert revision == plan["execution_sha"], (revision, plan["execution_sha"])
    assert not subprocess.check_output(["git", "status", "--porcelain"], cwd=a.checkout)
    sources = []
    for app in plan["applications"]:
        archive = a.archives / f"{app['index']:02}.tar.gz"
        assert digest(archive) == app["archive_sha256"], app["name"]
        sources.append(archive)
    a.output.mkdir(parents=True, exist_ok=False)
    scratch = a.output / "scratch"
    scratch.mkdir()
    env = dict(os.environ, TMPDIR=str(scratch.resolve()))
    tools = {n: (a.bin / n).resolve() for n in ("coverage_baseline", "workspace_proposal", "ato-formation-worker")}
    records = []
    for app, archive in zip(plan["applications"], sources):
        n = f"{app['index']:02}"
        sha = "sha256:" + app["archive_sha256"]
        a_out = a.output / f"{n}-A.json"
        run([tools["coverage_baseline"], archive.resolve(), sha, scratch / f"{n}-A",
             tools["ato-formation-worker"], a_out], a.output / f"{n}-A.log", env)
        b_out = a.output / f"{n}-B.json"
        run([tools["workspace_proposal"], archive.resolve(), sha, scratch / f"{n}-B",
             tools["ato-formation-worker"], "denied", "none", b_out], a.output / f"{n}-B.log", env)
        b = json.loads(b_out.read_text())
        runs = []
        ids = []
        if b.get("operation_published"):
            ids = b["provider_request"]["operation_catalog"]["operations"][0]["workspace_ids"]
        for wid in ids:
            out = a.output / f"{n}-B-{wid}.json"
            run([tools["workspace_proposal"], archive.resolve(), sha, scratch / f"{n}-B-{wid}",
                 tools["ato-formation-worker"], "denied", wid, out], a.output / f"{n}-B-{wid}.log", env)
            runs.append({"workspace_id": wid, "sha256": digest(out), "record": json.loads(out.read_text())})
        records.append({**app, "A": {"sha256": digest(a_out), "observation": json.loads(a_out.read_text())},
                        "B": {"sha256": digest(b_out), "inventory_record": b, "selected_runs": runs}})
        print(json.dumps({"app": app["name"], "A": records[-1]["A"]["observation"]["result"].get("status"),
                          "B_terminal": b.get("terminal"), "ids": ids}), flush=True)
    result = {"schema": "ato.formation-node-static-workspace-remeasurement-results/1",
              "execution_sha": revision, "plan_sha256": digest(a.plan),
              "binary_hashes": {k: digest(v) for k, v in tools.items()},
              "applications": records, "model_calls": 0, "network": "denied"}
    (a.output / "results.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
