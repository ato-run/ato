#!/usr/bin/env python3
"""6b-C classification rules, fixed before reading the 50-app results."""
STAGE_ORDER = {"source": 0, "preset": 1, "authoring": 1, "projection": 2, "plan": 2, "admission": 3,
               "build": 4, "execution": 4, "run": 4, "realization": 4, "verification": 5, "publication": 6}
def primary_of(code, stage):
    if code == "network_denied": return "effect/policy"
    if code.startswith(("preset_", "capsule_toml", "authoring_", "workspace_")) or code in (
            "intent_requires_authoring", "intent_no_lane", "intent_ambiguous_lockfiles", "intent_malformed"):
        return "known-D/authoring"
    if code.startswith(("intent_unsupported", "package_manager_version", "toolchain", "runtime_cannot")):
        return "runtime/toolchain"
    if stage in ("build", "execution", "run", "realization"): return "build-capability"
    if stage == "verification": return "verifier"
    if stage in ("preset", "authoring", "projection", "plan"): return "known-D/authoring"
    return "UNKNOWN"
def layers(obs):
    r = obs["result"] if obs else {"status": "unclassified_error"}
    L = dict.fromkeys("ABCDEFGHI", False)
    if r.get("status") != "formation_result":
        return L, [], r
    res = r["result"]; atts = res.get("attempts", [])
    L["A"] = True
    for a in atts:
        f = a.get("failure") or {}
        L["B"] |= bool(a.get("contract_ref")); L["C"] |= bool(a.get("derivation_ref"))
        admitted = bool(a.get("derivation_ref")) and f.get("stage") not in ("preset", "projection", "plan", "admission", "authoring")
        L["D"] |= admitted
        ran = (a.get("outcomes", {}).get("cleanup", {}).get("state") not in (None, "not_applicable"))
        L["E"] |= ran
        L["F"] |= a.get("outcomes", {}).get("runtime_verification", {}).get("state") not in (None, "not_attempted") or bool(a.get("verification"))
        L["G"] |= bool((a.get("receipt") or {}).get("fully_satisfied"))
    L["H"] = any(v.get("materialization_ref") for v in res.get("verified_routes", []))
    return L, atts, r
def classify(obs, process):
    L, atts, r = layers(obs)
    if "timeout_seconds" in process: return "UNKNOWN", "hard_timeout", L
    if r.get("status") == "typed_terminal_error":
        code = r["code"]; return ("Adapter" if r.get("stage") == "source" else primary_of(code, r.get("stage"))), code, L
    if r.get("status") != "formation_result": return "UNKNOWN", "unclassified_error", L
    if L["G"]: return "success", "fully_satisfied", L
    best = max(atts, key=lambda a: STAGE_ORDER.get((a.get("failure") or {}).get("stage"), 0), default=None)
    if not best or not best.get("failure"): return "UNKNOWN", "no_attempt_failure", L
    f = best["failure"]; return primary_of(f["code"], f.get("stage")), f["code"], L
