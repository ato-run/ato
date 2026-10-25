#!/usr/bin/env python3
"""6b-C: 50-app actual Formation measurement, known-D only, no model.

Original 20 (6a plan, unchanged archives) + preregistered 30. Every archive
hash is checked before the first Formation invocation. Rust sources of the
checkout must equal the merged measurement pin (harness/docs may differ).
"""
import argparse, hashlib, json, os, subprocess
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser()
    for name in ("checkout", "old_archives", "new_archives", "bin", "output"):
        p.add_argument("--" + name.replace("_", "-"), dest=name, type=Path, required=True)
    a = p.parse_args()
    plan_path = a.checkout / "docs/ops/formation-coverage-50-plan.json"
    plan = json.loads(plan_path.read_text())
    assert plan["schema"] == "ato.formation-coverage-50-plan/1" and not plan["selection_outcomes_observed"]
    policy = plan["measurement_policy"]
    assert policy["network"] == "denied" and policy["model_calls"] == 0
    assert policy["candidate_producer"] == policy["decision_provider"] == "off"
    old_plan = json.loads((a.checkout / plan["original_20"]["plan"]).read_text())
    assert digest(a.checkout / plan["original_20"]["plan"]) == plan["original_20"]["plan_sha256"]
    assert not subprocess.check_output(["git", "status", "--porcelain"], cwd=a.checkout)
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=a.checkout, text=True).strip()
    pin = plan["measurement_code"]["pin"]
    rust_diff = subprocess.check_output(
        ["git", "diff", "--name-only", pin, head, "--", "*.rs", "Cargo.lock", "**/Cargo.toml"],
        cwd=a.checkout, text=True).strip()
    assert not rust_diff, rust_diff
    cohort = [(app, a.old_archives / f"{app['index']:02}.tar.gz", "original_20") for app in old_plan["applications"]]
    cohort += [(app, a.new_archives / f"{app['index']}.tar.gz", "new_30") for app in plan["applications"]]
    assert len(cohort) == 50 and len({app["index"] for app, _, _ in cohort}) == 50
    for app, archive, _ in cohort:
        assert digest(archive) == app["archive_sha256"], app["name"]
    # All 50 source identities confirmed; only now does Formation run.
    a.output.mkdir(parents=True, exist_ok=False)
    scratch = a.output / "scratch"
    scratch.mkdir()
    env = dict(os.environ, TMPDIR=str(scratch.resolve()))
    helper, worker = (a.bin / "coverage_baseline").resolve(), (a.bin / "ato-formation-worker").resolve()
    records = []
    timeout = policy["hard_process_timeout_seconds"]
    for app, archive, group in cohort:
        n = f"{app['index']:02}"
        out, log = a.output / f"{n}.json", a.output / f"{n}.log"
        with log.open("x") as stream:
            try:
                done = subprocess.run([helper, archive.resolve(), "sha256:" + app["archive_sha256"],
                                       scratch / n, worker, out], env=env, stdout=stream,
                                      stderr=subprocess.STDOUT, timeout=timeout)
                status = {"exit": done.returncode}
            except subprocess.TimeoutExpired:
                status = {"timeout_seconds": timeout}
        observation = json.loads(out.read_text()) if out.exists() else None
        if observation is not None:
            assert observation["model_calls"] == 0
        records.append({"index": app["index"], "name": app["name"], "group": group, "process": status,
                        "observation_sha256": digest(out) if out.exists() else None,
                        "observation": observation})
        subprocess.run(["rm", "-rf", str(scratch / n)])
        print(json.dumps({"app": app["name"], "process": status,
                          "status": observation and observation["result"].get("status")}), flush=True)
    result = {"schema": "ato.formation-coverage-50-results/1", "checkout": head, "measurement_pin": pin,
              "plan_sha256": digest(plan_path), "binary_hashes": {"coverage_baseline": digest(helper),
              "ato-formation-worker": digest(worker)}, "applications": records, "model_calls": 0,
              "network": "denied"}
    (a.output / "results.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
