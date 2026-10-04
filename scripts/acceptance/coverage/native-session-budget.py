#!/usr/bin/env python3
"""Owner acceptance accounting; reads existing CLI views, never starts a Search."""
import argparse
import json
import subprocess
from pathlib import Path

FIELDS = ("D_rounds", "exchanges", "Runtime_attempts", "inspections", "sequential_elapsed_seconds")

def consumption(view):
    progress = view.get("progress", {})
    elapsed = view.get("search_elapsed_ms")
    if type(elapsed) is not int or elapsed < 0:
        raise ValueError("incomplete_budget_evidence")
    attempts = progress.get("search_budget", {}).get("attempts", {})
    if any(type(attempts.get(k)) is not int or attempts[k] < 0 for k in ("used", "reserved")):
        raise ValueError("incomplete_budget_evidence")
    values = {
        "D_rounds": progress.get("rounds_consumed"),
        "exchanges": view.get("exchanges_used"),
        "Runtime_attempts": sum(attempts[k] for k in ("used", "reserved")),
        "inspections": view.get("inspection_exchanges_completed"),
        "sequential_elapsed_seconds": (elapsed + 999) // 1000,
    }
    if (not isinstance(view.get("search_id"), str)
            or not isinstance(view.get("configuration_ref"), str)
            or not view["search_id"] or not view["configuration_ref"]
            or type(view.get("deadline_ms")) is not int or view["deadline_ms"] <= 0
            or any(type(v) is not int or v < 0 for v in values.values())):
        raise ValueError("incomplete_budget_evidence")
    return values

def audit(plan, views, unregistered_identifiers=0, requested=None):
    requested = requested or {}
    ceilings = plan["aggregate_ceiling"]
    keys = (*FIELDS, "searches")
    if (type(unregistered_identifiers) is not int or unregistered_identifiers < 0
            or not isinstance(requested, dict) or any(k not in keys for k in requested)
            or any(type(v) is not int or v < 0 for v in requested.values())
            or any(type(ceilings.get(k)) is not int or ceilings[k] < 0 for k in keys)
            or not isinstance(plan.get("measurement_id"), str) or not plan["measurement_id"]):
        raise ValueError("budget_arguments_invalid")
    by_search = {}
    for view in views:
        sid = view["search_id"]
        used = consumption(view)
        identity = (view["configuration_ref"], view.get("deadline_ms"))
        previous = by_search.get(sid)
        if previous and previous["binding"] != identity:
            raise ValueError("historical_Search_binding_changed")
        # A reconnect, cancellation or older snapshot cannot reduce spent scope.
        merged = {k: max(used[k], previous["used"][k] if previous else 0) for k in FIELDS}
        by_search[sid] = {"binding": identity, "used": merged}
    totals = {k: sum(v["used"][k] for v in by_search.values()) for k in FIELDS}
    totals["searches"] = len(by_search) + unregistered_identifiers
    ceilings = plan["aggregate_ceiling"]
    exceeded = {k: {"used": v, "requested": requested.get(k, 0), "max": ceilings[k]}
                for k, v in totals.items() if v + requested.get(k, 0) > ceilings[k]}
    return {
        "schema": "ato.native-acceptance-budget-audit/1",
        "measurement_id": plan["measurement_id"],
        "totals": totals, "requested": requested,
        "limits": {k: ceilings[k] for k in totals}, "exceeded": exceeded,
        "admitted": not exceeded,
        "round_basis": "Coordinator allocated rounds, including unanswered cancelled/deadline rounds",
        "direct_API_ledger_modified": False,
        "Search_started_or_reset": False,
        "historical_Source_successes_or_counters_rewritten": False,
    }

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--ato", type=Path)
    parser.add_argument("--connection", type=Path, action="append", default=[])
    parser.add_argument("--saved-status", type=Path, action="append", default=[])
    parser.add_argument("--unregistered-identifiers", type=int, default=0)
    for name in (*FIELDS, "searches"):
        parser.add_argument("--reserve-" + name.replace("_", "-"), type=int, default=0)
    args = parser.parse_args()
    try:
        if args.unregistered_identifiers < 0 or (args.connection and not args.ato):
            raise ValueError("budget_arguments_invalid")
        views = [json.loads(p.read_text()) for p in args.saved_status]
        for path in args.connection:
            result = subprocess.run([str(args.ato), "form-session", "--connection", str(path), "status"],
                                    capture_output=True, timeout=10, check=True)
            views.append(json.loads(result.stdout))
        if not views:
            raise ValueError("budget_evidence_required")
        requested = {k: getattr(args, "reserve_" + k) for k in (*FIELDS, "searches")}
        if any(v < 0 for v in requested.values()):
            raise ValueError("budget_arguments_invalid")
        report = audit(json.loads(args.plan.read_text()), views, args.unregistered_identifiers, requested)
        print(json.dumps(report, indent=2))
        return 0 if report["admitted"] else 2
    except (ValueError, KeyError, OSError, subprocess.SubprocessError):
        # Do not disclose owner paths, credentials or malformed input excerpts.
        print(json.dumps({"error": "native_budget_evidence_unavailable", "admitted": False}))
        return 1

if __name__ == "__main__":
    raise SystemExit(main())
