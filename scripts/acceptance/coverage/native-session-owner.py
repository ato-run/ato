#!/usr/bin/env python3
"""Owner campaign admission at Search creation and Native launch boundaries.

This is a thin wrapper over ato form/form-session and the common Native launcher.
It is not a Coordinator budget policy. A lost start retains its full reservation.
"""
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

SPEC = importlib.util.spec_from_file_location("campaign_budget", Path(__file__).with_name("native-session-budget.py"))
BUDGET = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUDGET)
KEYS = (*BUDGET.FIELDS, "searches")
TERMINAL = {"stopped", "cancelled", "failed", "unsatisfied", "exhausted", "satisfied",
            "k_reached_awaiting_assessment"}


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def source_declaration_pass(view):
    """A product preset may finish before any Native exchange is available."""
    progress = view.get("progress", {})
    receipt = progress.get("receipt")
    attempts = progress.get("search_budget", {}).get("attempts", {})
    return (progress.get("status") in ("satisfied", "k_reached_awaiting_assessment")
            and progress.get("rounds_consumed") == 0 and view.get("exchanges_used") == 0
            and isinstance(receipt, dict) and receipt.get("fully_satisfied") is True
            and receipt.get("status") == "k_reached_awaiting_assessment"
            and isinstance(receipt.get("attempt_id"), str) and bool(receipt["attempt_id"])
            and receipt.get("contract_ref") == progress.get("contract_ref")
            and type(attempts.get("used")) is int and attempts["used"] > 0)


def pre_exchange_infrastructure_terminal(view):
    """A failed input preparation must not require a reporting-only Native turn."""
    progress = view.get("progress", {})
    attempts = progress.get("search_budget", {}).get("attempts", {})
    return (progress.get("status") == "unsatisfied"
            and progress.get("termination_reason") == "infrastructure_failure"
            and "input" in view and view["input"] is None
            and type(view.get("exchanges_used")) is int and view["exchanges_used"] == 0
            and type(view.get("inspection_exchanges_completed")) is int
            and view["inspection_exchanges_completed"] == 0
            and type(progress.get("rounds_consumed")) is int
            and progress["rounds_consumed"] > 0
            and type(progress.get("unresolved_attempts")) is int
            and progress["unresolved_attempts"] == 0
            and progress.get("receipt") is None
            and type(attempts.get("used")) is int and attempts["used"] == 0
            and type(attempts.get("reserved")) is int and attempts["reserved"] == 0)


def native_completion_evidence(entry):
    if entry.get("native_launch_in_progress", False) is not False:
        return False
    if type(entry.get("native_exit_code")) is int:
        return True
    if "native_started_at_ms" in entry:
        return False
    view = entry["snapshots"][0]
    return ((entry.get("native_not_required") == "source_declaration_PASS"
             and source_declaration_pass(view))
            or (entry.get("native_not_required") == "pre_exchange_infrastructure_terminal"
                and pre_exchange_infrastructure_terminal(view)))


def load_plan(path):
    plan = json.loads(path.read_text())
    BUDGET.audit(plan, [])  # Strict ceiling/request types, including empty plans.
    if plan.get("schema") != "ato.native-acceptance-campaign/1":
        raise ValueError("campaign_plan_required")
    return plan


def publish(path, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as staged:
        temporary = Path(staged.name)
        try:
            json.dump(value, staged, indent=2)
            staged.write("\n")
            staged.flush()
            os.fsync(staged.fileno())
            os.replace(temporary, path)
            descriptor = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        finally:
            temporary.unlink(missing_ok=True)


@contextmanager
def locked(path):
    # Retain the lock file: unlinking it would allow two distinct locked inodes.
    descriptor = os.open(path.with_suffix(path.suffix + ".lock"), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def initialize(plan, ledger):
    if ledger.exists() or ledger.is_symlink():
        raise ValueError("campaign_ledger_already_exists")
    if "historical_audit" in plan:
        history = plan["historical_audit"]
        body = Path(history["path"]).read_bytes()
        if hashlib.sha256(body).hexdigest() != history["sha256"]:
            raise ValueError("campaign_history_changed")
        report = json.loads(body)
        floor = report["totals"]
        if report.get("schema") != "ato.native-acceptance-budget-audit/1":
            raise ValueError("campaign_history_required")
    elif plan.get("initial_campaign") is True:
        floor = {k: 0 for k in KEYS}
    else:
        raise ValueError("campaign_history_required")
    if any(type(floor.get(k)) is not int or floor[k] < 0 for k in KEYS):
        raise ValueError("campaign_history_incomplete")
    value = {"schema": "ato.native-acceptance-campaign-ledger/1", "measurement_id": plan["measurement_id"],
             "plan_sha256": fingerprint(plan), "historical_floor": floor, "entries": []}
    publish(ledger, value)
    return value


def read_ledger(plan, path):
    if path.is_symlink():
        raise ValueError("campaign_ledger_invalid")
    value = json.loads(path.read_text())
    if (value.get("schema") != "ato.native-acceptance-campaign-ledger/1"
            or value.get("plan_sha256") != fingerprint(plan)
            or value.get("measurement_id") != plan["measurement_id"]):
        raise ValueError("campaign_binding_changed")
    return value


def scope(config, attempts, seconds):
    exploration = config["exploration"]
    if (config.get("schema") != "ato.formation-exploration-config/2"
            or config["provider"]["provider"] != "agent_session"):
        raise ValueError("native_session_config_required")
    values = {"searches": 1, "D_rounds": exploration["formation"]["max_rounds"],
              "exchanges": max(exploration["max_provider_calls"], config["provider_budget"]["max_calls"]),
              "inspections": exploration["max_inspections"], "Runtime_attempts": attempts,
              "sequential_elapsed_seconds": seconds}
    if any(type(v) is not int or v < 0 for v in values.values()) or attempts <= 0 or seconds <= 0:
        raise ValueError("campaign_scope_invalid")
    if values["D_rounds"] <= 0 or values["exchanges"] <= 0:
        raise ValueError("campaign_scope_invalid")
    return values


def refresh(plan, ledger, ato, requested=None, reader=None):
    requested = requested or {}
    totals = dict(ledger["historical_floor"])
    held = {k: 0 for k in KEYS}
    if any(type(totals.get(k)) is not int or totals[k] < 0 for k in KEYS):
        raise ValueError("campaign_history_incomplete")
    seen = set()
    for entry in ledger["entries"]:
        if entry.get("start_state") != "launched":
            # A crash between durable reservation and process creation is not free.
            raise ValueError("campaign_start_requires_reconciliation")
        connection = Path(entry["connection"])
        seal = entry.get("terminal_retirement")
        if seal is not None:
            if (seal.get("schema") != "ato.native-terminal-retirement/1"
                    or len(entry["snapshots"]) != 1
                    or seal.get("snapshot_sha256") != fingerprint(entry["snapshots"][0])
                    or seal.get("descriptor_sha256") != fingerprint(json.loads(connection.read_text()))
                    or not native_completion_evidence(entry)):
                raise ValueError("campaign_terminal_evidence_changed")
            view = entry["snapshots"][0]
            if view["progress"]["status"] not in TERMINAL or view.get("input") is not None:
                raise ValueError("campaign_terminal_evidence_changed")
        elif reader:
            view = reader(connection)
        else:
            result = subprocess.run([str(ato), "form-session", "--connection", str(connection), "status"],
                                    capture_output=True, check=True, timeout=10)
            view = json.loads(result.stdout)
        history = entry["snapshots"] + [view]
        report = BUDGET.audit(plan, history)
        if report["totals"]["searches"] != 1 or view["search_id"] in seen:
            raise ValueError("campaign_Search_binding_changed")
        seen.add(view["search_id"])
        used = report["totals"]
        if any(used[k] > entry["reservation"][k] for k in KEYS):
            # Unexpected consumption cannot release this reservation or launch work.
            raise ValueError("campaign_scope_exceeded")
        for k in KEYS:
            totals[k] += used[k]
            if view["progress"]["status"] not in TERMINAL:
                held[k] += max(0, entry["reservation"][k] - used[k])
        # Persist only the maximum dimensions and original binding, not raw logs.
        snapshot = dict(view)
        snapshot["progress"] = dict(view["progress"])
        snapshot["progress"]["rounds_consumed"] = used["D_rounds"]
        snapshot["progress"]["search_budget"] = {"attempts": {"used": used["Runtime_attempts"], "reserved": 0}}
        snapshot["exchanges_used"] = used["exchanges"]
        snapshot["inspection_exchanges_completed"] = used["inspections"]
        snapshot["search_elapsed_ms"] = used["sequential_elapsed_seconds"] * 1000
        if seal is None:
            entry["snapshots"] = [snapshot]
    ceilings = plan["aggregate_ceiling"]
    if any(k not in KEYS or type(v) is not int or v < 0 for k, v in requested.items()):
        raise ValueError("campaign_scope_invalid")
    exceeded = {k: {"used": totals[k], "held": held[k], "requested": requested.get(k, 0), "max": ceilings[k]}
                for k in KEYS if totals[k] + held[k] + requested.get(k, 0) > ceilings[k]}
    return {"schema": "ato.native-acceptance-start-admission/1", "measurement_id": plan["measurement_id"],
            "used": totals, "held": held, "requested": requested, "exceeded": exceeded,
            "admitted": not exceeded, "direct_API_ledger_modified": False}


def guarded_start(plan, ledger_path, args, spawn=subprocess.Popen):
    with locked(ledger_path):
        ledger = read_ledger(plan, ledger_path)
        config = json.loads(args.exploration_config.read_text())
        wall_seconds = getattr(args, "wall_clock_seconds", None) or args.deadline_seconds
        if type(wall_seconds) is not int or wall_seconds < args.deadline_seconds:
            raise ValueError("campaign_wall_clock_invalid")
        requested = scope(config, args.max_attempts, wall_seconds)
        report = refresh(plan, ledger, args.ato, requested)
        if not report["admitted"]:
            publish(ledger_path, ledger)
            return report
        # Fresh paths prevent using this admission to resume/reset another Search.
        if any(p.exists() or p.is_symlink() for p in (args.connection, args.work_root)):
            raise ValueError("fresh_Search_paths_required")
        entry = {"id": str(uuid.uuid4()), "reservation": requested, "start_state": "prepared",
                 "config_sha256": fingerprint(config), "connection": str(args.connection.absolute()),
                 "Search_deadline_seconds": args.deadline_seconds, "snapshots": []}
        ledger["entries"].append(entry)
        publish(ledger_path, ledger)  # Write-ahead reservation before Search create.
        # The real CLI reads the admitted bytes, not a mutable planning file.
        frozen_config = ledger_path.with_name(entry["id"] + ".config.json")
        with frozen_config.open("x") as staged:
            os.chmod(frozen_config, 0o600)
            json.dump(config, staged)
            staged.flush()
            os.fsync(staged.fileno())
        command = [str(args.ato), "form", str(args.source), "--runtime-network",
                   "--exploration-config", str(frozen_config), "--api", args.api,
                   "--token-file", str(args.token_file), "--exact-runtime", args.exact_runtime,
                   "--network", "denied", "--work-root", str(args.work_root),
                   "--max-attempts", str(args.max_attempts), "--deadline-seconds", str(args.deadline_seconds),
                   "--max-transfer-bytes", str(args.max_transfer_bytes),
                   "--max-expanded-bytes", str(args.max_expanded_bytes),
                   "--max-stored-bytes", str(args.max_stored_bytes), "--session-bridge", str(args.connection)]
        if fingerprint(json.loads(args.exploration_config.read_text())) != entry["config_sha256"]:
            raise ValueError("campaign_config_changed")
        with ledger_path.with_name(entry["id"] + ".owner-log").open("xb") as log:
            process = spawn(command, stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)
        entry.update(start_state="launched", pid=process.pid)
        publish(ledger_path, ledger)
        return dict(report, start_id=entry["id"], Search_process_started=True, Search_reset=False)


def guarded_native(plan, ledger_path, args, execute=subprocess.run):
    with locked(ledger_path):
        ledger = read_ledger(plan, ledger_path)
        report = refresh(plan, ledger, args.ato)
        publish(ledger_path, ledger)
        if not report["admitted"]:
            return report
        entries = [e for e in ledger["entries"] if Path(e["connection"]).resolve() == args.connection.resolve()]
        if len(entries) != 1:
            raise ValueError("registered_Search_required")
        entry = entries[0]
        if entry.get("terminal_retirement") is not None:
            raise ValueError("campaign_Search_retired")
        if entry.get("native_launch_in_progress", False) is not False:
            raise ValueError("campaign_Native_launch_requires_reconciliation")
        native_args = args.native_args[1:] if args.native_args[:1] == ["--"] else args.native_args
        if any(a == "--connection" or a.startswith("--connection=") for a in native_args):
            raise ValueError("connection_is_owner_fixed")
        if any(a == "--owner-wall-clock-deadline-ms" or a.startswith("--owner-wall-clock-deadline-ms=")
               for a in native_args):
            raise ValueError("wall_clock_is_owner_fixed")
        if "--reconcile-only" in native_args:
            entry = entries[0]
            seconds = entry.get("Search_deadline_seconds")
            if type(seconds) is not int or seconds <= 0 or not entry.get("snapshots"):
                raise ValueError("campaign_clock_evidence_required")
            original_deadline = entry["snapshots"][0]["deadline_ms"]
            wall_deadline = original_deadline + (
                entry["reservation"]["sequential_elapsed_seconds"] - seconds) * 1000
            if wall_deadline <= time.time() * 1000:
                raise ValueError("campaign_wall_clock_exceeded")
            native_args = [*native_args, "--owner-wall-clock-deadline-ms", str(wall_deadline)]
        # Serialize Native launches too; reconnect is not a second aggregate slot.
        entry.setdefault("native_started_at_ms", int(time.time() * 1000))
        entry["native_launch_in_progress"] = True
        publish(ledger_path, ledger)  # A lost Native launch cannot become "not started".
        result = execute([sys.executable, str(args.native_launcher), "--connection", str(args.connection),
                          *native_args], check=False)
        entry["native_exit_code"] = result.returncode
        entry["native_finished_at_ms"] = int(time.time() * 1000)
        entry["native_launch_in_progress"] = False
        publish(ledger_path, ledger)
        return dict(report, Native_exit_code=result.returncode, Search_reset=False)


def retire_search(plan, ledger_path, args, reader=None):
    """Freeze live terminal evidence before its owner closes the Bridge.

    A cached snapshot alone cannot retire a Search. Unresolved attempts or
    reserved resources fail closed. This never retires a prepared/lost start.
    """
    with locked(ledger_path):
        ledger = read_ledger(plan, ledger_path)
        entries = [e for e in ledger["entries"]
                   if Path(e["connection"]).resolve() == args.connection.resolve()]
        if len(entries) != 1:
            raise ValueError("registered_Search_required")
        entry = entries[0]
        if entry.get("terminal_retirement") is not None:
            raise ValueError("campaign_Search_retired")
        # Read the actual live Bridge, not a previous terminal-looking cache.
        def terminal_reader(connection):
            if reader:
                view = reader(connection)
            else:
                result = subprocess.run([str(args.ato), "form-session", "--connection",
                                         str(connection), "status"], capture_output=True,
                                        check=True, timeout=10)
                view = json.loads(result.stdout)
            if connection.resolve() == args.connection.resolve():
                if entry.get("native_launch_in_progress", False) is not False:
                    raise ValueError("campaign_Native_completion_required")
                descriptor = json.loads(connection.read_text())
                progress = view["progress"]
                if (progress["status"] not in TERMINAL or "input" not in view or view["input"] is not None
                        or type(progress.get("unresolved_attempts")) is not int
                        or progress["unresolved_attempts"] != 0
                        or any(not isinstance(v, dict) or type(v.get("reserved")) is not int or v["reserved"] != 0
                               for v in progress["search_budget"].values())
                        or view["search_id"] != descriptor["search_id"]
                        or view["configuration_ref"] != descriptor["configuration_ref"]):
                    raise ValueError("campaign_terminal_reconciliation_required")
                if type(entry.get("native_exit_code")) is not int:
                    if "native_started_at_ms" in entry:
                        raise ValueError("campaign_Native_completion_required")
                    if source_declaration_pass(view):
                        entry["native_not_required"] = "source_declaration_PASS"
                    elif pre_exchange_infrastructure_terminal(view):
                        entry["native_not_required"] = "pre_exchange_infrastructure_terminal"
                    else:
                        raise ValueError("campaign_Native_completion_required")
                    # Preserve allocated rounds. Never fabricate a Native exit code.
            return view
        report = refresh(plan, ledger, args.ato, reader=terminal_reader)
        if not report["admitted"]:
            publish(ledger_path, ledger)
            return report
        if not native_completion_evidence(entry):
            raise ValueError("campaign_Native_completion_required")
        entry["terminal_retirement"] = {
            "schema": "ato.native-terminal-retirement/1",
            "snapshot_sha256": fingerprint(entry["snapshots"][0]),
            "descriptor_sha256": fingerprint(json.loads(args.connection.read_text())),
            "retired_at_ms": int(time.time() * 1000),
        }
        publish(ledger_path, ledger)
        return dict(report, terminal_Search_retired=True, Search_reset=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--ledger", type=Path, required=True)
    parser.add_argument("--ato", type=Path, required=True)
    commands = parser.add_subparsers(dest="operation", required=True)
    commands.add_parser("initialize")
    commands.add_parser("audit")
    retire = commands.add_parser("retire")
    retire.add_argument("--connection", type=Path, required=True)
    start = commands.add_parser("start")
    for name in ("source", "exploration-config", "token-file", "work-root", "connection"):
        start.add_argument("--" + name, type=Path, required=True)
    start.add_argument("--api", required=True)
    start.add_argument("--exact-runtime", required=True)
    start.add_argument("--max-attempts", type=int, default=4)
    start.add_argument("--deadline-seconds", type=int, default=1800)
    start.add_argument("--wall-clock-seconds", type=int,
                       help="campaign reservation including reporting; never changes Search deadline")
    for name in ("transfer", "expanded", "stored"):
        start.add_argument("--max-" + name + "-bytes", type=int, required=True)
    native = commands.add_parser("native")
    native.add_argument("--connection", type=Path, required=True)
    native.add_argument("--native-launcher", type=Path, required=True)
    native.add_argument("native_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    try:
        plan = load_plan(args.plan)
        if args.operation == "initialize":
            with locked(args.ledger):
                initialize(plan, args.ledger)
            report = {"initialized": True, "Search_started": False}
        elif args.operation == "start":
            report = guarded_start(plan, args.ledger, args)
        elif args.operation == "native":
            report = guarded_native(plan, args.ledger, args)
        elif args.operation == "retire":
            report = retire_search(plan, args.ledger, args)
        else:
            with locked(args.ledger):
                ledger = read_ledger(plan, args.ledger)
                report = refresh(plan, ledger, args.ato)
                publish(args.ledger, ledger)
        print(json.dumps(report))
        return 0 if report.get("admitted", True) else 2
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError):
        print(json.dumps({"error": "native_campaign_evidence_unavailable", "admitted": False,
                          "Search_reset": False}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
