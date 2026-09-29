#!/usr/bin/env python3
"""6b-D1 ledger: native-process route vs Dockerfile->OCI route, classification
only. Inputs: the corrected D0 ledger and the parsed Dockerfile facts."""
import json, re, sys, collections
from pathlib import Path

# Semantic judgments that a parser cannot make, each with its evidence.
OVERRIDES = {
    39: ("requires_binding", "ENTRYPOINT reads /app/config/glance.yml, which the final stage does not copy (only the binary): a config Binding is required"),
    35: ("requires_binding", "settings.yaml defaults select a RabbitMQ task broker (PGPT_TASKS_RESULTS_BROKER_MODE:rabbitmq) and an external LLM; startup without them is unverified"),
}
FLAGS = {
    19: ["runtime sandbox (gVisor via sandbox/run.sh) may assume extra container privileges; unverified"],
    28: ["shell-form CMD runs `npm run migrate && npm start` inside the container"],
    7: ["FROM uses ARG PYTHON_VERSION with default 3.11"],
    8: ["final stage chosen by ARG BUILD_ENV=git default (build_${BUILD_ENV})"],
}
HTTP_PORT = re.compile(r"^[0-9]{2,5}(/tcp)?$")


def oci_route(app, facts, external):
    files = facts["dockerfiles"]
    if external:
        return "blocked_external_service", None
    if not files:
        return "no_dockerfile", None
    root = next((f for f in files if f["path"] == "Dockerfile"), None)
    if root is None:
        return "ambiguous_selection", None  # no docker-default root Dockerfile; context/target undefined
    p, fin = root["parsed"], root["parsed"]["final"]
    if p["arg_in_from"]:
        defaults = {a.split("=", 1)[0] for a in p["global_args"] if "=" in a}
        used = set(re.findall(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)", " ".join(p["bases"])))
        if not used <= defaults or any("${TARGETPLATFORM}" in a or "${BUILDPLATFORM}" in a for a in p["global_args"]):
            return "ambiguous_build_args", p
    if p["remote_add"] or p["privileged"] or {"secret", "ssh"} & set(p["mounts"]):
        return "unsafe_build_features", p
    if not (fin["cmd"] or fin["entrypoint"]):
        return "no_launch", p
    ports = fin["expose"]
    if len(ports) != 1:
        return "ambiguous_port" if ports else "no_port", p
    if not HTTP_PORT.match(ports[0]):
        return "ambiguous_port", p  # variable or non-TCP; v0 requires a literal
    if app["index"] in OVERRIDES:
        return OVERRIDES[app["index"]][0], p
    return "v0_eligible", p


def native_route(app):
    if app["external_service_required"]:
        return "blocked_external_service"
    if not app["runtime_in_current_catalog"]:
        return "blocked_runtime_catalog"
    if not app["typed_native_launch"]:
        return "blocked_launch_undeclared"
    return "blocked_port_binding"  # 0/41 have a native port contract


def main():
    d0 = json.loads(Path(sys.argv[1]).read_text())
    facts = json.loads(Path(sys.argv[2]).read_text())
    rows = []
    for a in d0["applications"]:
        f = facts[str(a["index"])]
        oci, p = oci_route(a, f, a["external_service_required"])
        nat = native_route(a)
        # Primary bucket precedence (fixed before counting):
        # external service > OCI v0 eligible > native blocked only by port binding
        # > runtime catalog > everything else ambiguous. No route is possible now.
        if a["external_service_required"]:
            primary = "blocked_external_service"
        elif oci == "v0_eligible":
            primary = "dockerfile_oci_possible_if_build_added"
        elif nat == "blocked_port_binding":
            primary = "blocked_port_binding"
        elif nat == "blocked_runtime_catalog":
            primary = "blocked_runtime_catalog"
        else:
            primary = "ambiguous"
        root = next((x for x in f["dockerfiles"] if x["path"] == "Dockerfile"), None)
        rp = root["parsed"] if root else None
        rows.append({
            "index": a["index"], "name": a["name"], "family": a["family_name"],
            "primary_runtime_kind": a["primary_runtime_kind"], "runtime_in_current_catalog": a["runtime_in_current_catalog"],
            "native_route": nat, "oci_route": oci, "primary_bucket": primary,
            "dependency_network_gate": True,
            "dockerfiles": [x["path"] for x in f["dockerfiles"]],
            "selected_dockerfile": "Dockerfile" if root else None,
            "root_dockerfile": rp and {
                "stages": rp["stages"], "multi_stage": rp["stages"] > 1, "final_stage_is_default_target": True,
                "all_bases_digest_pinned": rp["all_bases_digest_pinned"], "arg_in_from": rp["arg_in_from"],
                "global_args": rp["global_args"], "remote_add": rp["remote_add"], "buildkit_mounts": rp["mounts"],
                "privileged_or_host_network": rp["privileged"], "platforms": rp["platforms"],
                "run_instructions": rp["run_total"], "run_needing_network": rp["run_network"],
                "copy_sources_sample": rp["copy_sources_sample"], "final": rp["final"]},
            "external_services": a["external_services"],
            "flags": FLAGS.get(a["index"], []) + ([OVERRIDES[a["index"]][1]] if a["index"] in OVERRIDES else []),
            "build_network_requirement": (None if not rp else
                "base image pull by tag (no digest pin)" + (f"; {rp['run_network']} RUN steps install/fetch dependencies" if rp["run_network"] else "")
                if not rp["all_bases_digest_pinned"] else
                f"digest-pinned bases; {rp['run_network']} RUN steps install/fetch dependencies"),
        })
    dist = collections.Counter(r["primary_bucket"] for r in rows)
    buckets = ["native_process_possible_now", "dockerfile_oci_possible_if_build_added", "blocked_runtime_catalog",
               "blocked_port_binding", "blocked_external_service", "blocked_dependency_network", "ambiguous"]
    distribution = {b: dist.get(b, 0) for b in buckets}
    assert sum(distribution.values()) == len(rows) == 41
    eligible = [r for r in rows if r["primary_bucket"] == "dockerfile_oci_possible_if_build_added"]
    ledger = {
        "schema": "ato.formation-oci-source-qualification-d1/1", "wave": "6b-D1",
        "base": "233bda52e7e1f3c9acd4a67d41974f2152f87f9d",
        "inputs": {"d0": "docs/ops/formation-service-qualification-d0.json (revision 2b8e3fcf)",
                   "dockerfile_facts": "docs/ops/formation-oci-source-qualification-d1-facts.json"},
        "method": "static parse of every Dockerfile/Containerfile (depth<=4, no node_modules) in the same 41 pinned archives; no build, pull, run or network",
        "oci_v0_subset": ["docker-default root Dockerfile selected (no variant choice)", "final stage = default target",
                          "FROM ARGs only with in-file defaults; no platform-injected ARGs",
                          "no remote ADD, no privileged/host-network/device RUN, no secret/ssh mounts",
                          "final stage CMD or ENTRYPOINT (left to OCI semantics, never converted to native argv)",
                          "exactly one literal TCP EXPOSE", "no mandatory external service",
                          "no Binding needed for startup (e.g. config absent from the image)"],
        "primary_precedence": "external service > OCI v0 eligible > native blocked only by port binding > runtime catalog > ambiguous",
        "distribution": distribution,
        "native_route_distribution": dict(collections.Counter(r["native_route"] for r in rows).most_common()),
        "oci_route_distribution": dict(collections.Counter(r["oci_route"] for r in rows).most_common()),
        "oci_v0_eligible": [{"name": r["name"], "runtime": r["primary_runtime_kind"], "runtime_in_catalog": r["runtime_in_current_catalog"],
                             "port": r["root_dockerfile"]["final"]["expose"][0], "stages": r["root_dockerfile"]["stages"],
                             "digest_pinned": r["root_dockerfile"]["all_bases_digest_pinned"], "flags": r["flags"]} for r in eligible],
        "universal_gates": {"dependency_network": "41/41 need dependency resolution (native install or image build: base pull + RUN installs); network stays denied",
                            "base_image_identity": f"{sum(1 for r in rows if r['root_dockerfile'] and not r['root_dockerfile']['all_bases_digest_pinned'])} of {sum(1 for r in rows if r['root_dockerfile'])} root Dockerfiles pull bases by tag; an OCI build route must resolve and freeze digests as Ato-owned facts"},
        "not_decided_here": "6b-D2 target; no implementation, no network change",
        "model_calls": 0, "deploy": "none", "remote_migration": "none",
        "applications": sorted(rows, key=lambda r: r["index"])}
    Path(sys.argv[3]).write_text(json.dumps(ledger, indent=2) + "\n")
    print(json.dumps({k: ledger[k] for k in ("distribution", "native_route_distribution", "oci_route_distribution", "oci_v0_eligible", "universal_gates")}, indent=1))


if __name__ == "__main__":
    main()
