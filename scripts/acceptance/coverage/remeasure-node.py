#!/usr/bin/env python3
"""Remeasure the eight frozen qualification archives through local Formation.

No provider, dependency install or repo-specific route. Network stays denied.
The existing coverage_baseline Rust helper owns authoring/execution/verification.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--checkout", type=Path, required=True)
    parser.add_argument("--archives", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    plan = json.loads(args.plan.read_text())
    assert plan["schema"] == "ato.formation-node-build-remeasurement/1"
    assert plan["network"] == "denied"
    assert plan["candidate_producer"] == plan["decision_provider"] == "off"
    assert plan["model_calls"] == 0 and len(plan["applications"]) == 8
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=args.checkout, text=True).strip()
    assert revision == plan["execution_sha"]
    assert not subprocess.check_output(["git", "status", "--porcelain"], cwd=args.checkout)
    sources = []
    for app in plan["applications"]:
        archive = args.archives / f"{app['index']:02}.tar.gz"
        assert digest(archive) == app["archive_sha256"], app["name"]
        sources.append(archive)
    # All eight immutable source checks precede the first Formation invocation.
    args.output.mkdir(parents=True, exist_ok=False)
    scratch = args.output / "scratch"
    scratch.mkdir()
    binary_hashes = {"helper": digest(args.helper), "worker": digest(args.worker)}
    records = []
    for app, archive in zip(plan["applications"], sources):
        output = args.output / f"{app['index']:02}.json"
        log = args.output / f"{app['index']:02}.log"
        env = dict(os.environ)
        env["TMPDIR"] = str(scratch.resolve())
        with log.open("x") as stream:
            subprocess.run([str(args.helper.resolve()), str(archive.resolve()),
                            "sha256:" + app["archive_sha256"], str(scratch / str(app["index"])),
                            str(args.worker.resolve()), str(output)],
                           env=env, stdout=stream, stderr=subprocess.STDOUT,
                           timeout=360, check=True)
        observation = json.loads(output.read_text())
        assert observation["model_calls"] == 0
        assert observation["result"]["status"] != "unclassified_error"
        records.append({**app, "observation_sha256": digest(output), "observation": observation})
        print(json.dumps({"app": app["name"], "result": observation["result"]}), flush=True)
    result = {"schema": "ato.formation-node-build-remeasurement-results/1",
              "execution_sha": revision, "plan_sha256": digest(args.plan),
              "binary_hashes": binary_hashes, "applications": records,
              "model_calls": 0, "network": "denied"}
    (args.output / "results.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
